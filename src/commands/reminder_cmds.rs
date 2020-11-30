use regex_command_attr::command;

use serenity::{
    client::Context,
    http::CacheHttp,
    model::{
        channel::GuildChannel,
        channel::Message,
        id::{ChannelId, GuildId, UserId},
        misc::Mentionable,
        webhook::Webhook,
    },
    Result as SerenityResult,
};

use tokio::process::Command;

use crate::{
    check_subscription_on_message,
    consts::{
        CHARACTERS, DAY, HOUR, LOCAL_TIMEZONE, MAX_TIME, MINUTE, MIN_INTERVAL, PYTHON_LOCATION,
        REGEX_CHANNEL, REGEX_CHANNEL_USER, REGEX_CONTENT_SUBSTITUTION, REGEX_INTERVAL_COMMAND,
        REGEX_REMIND_COMMAND, THEME_COLOR,
    },
    framework::SendIterator,
    language_manager::LanguageManager,
    models::{ChannelData, GuildData, Timer, UserData},
    time_parser::TimeParser,
    SQLPool,
};

use chrono::{offset::TimeZone, NaiveDateTime};

use rand::{rngs::OsRng, seq::IteratorRandom};

use sqlx::{encode::Encode, MySql, MySqlPool, Type};

use std::str::from_utf8;

use num_integer::Integer;

use std::{
    collections::HashSet,
    convert::TryInto,
    default::Default,
    fmt::Display,
    string::ToString,
    time::{SystemTime, UNIX_EPOCH},
};

use regex::RegexBuilder;

fn shorthand_displacement(seconds: u64) -> String {
    let (days, seconds) = seconds.div_rem(&DAY);
    let (hours, seconds) = seconds.div_rem(&HOUR);
    let (minutes, seconds) = seconds.div_rem(&MINUTE);

    let time_repr = format!("{:02}:{:02}:{:02}", hours, minutes, seconds);

    if days > 0 {
        format!("{} days, {}", days, time_repr)
    } else {
        time_repr
    }
}

fn longhand_displacement(seconds: u64) -> String {
    let (days, seconds) = seconds.div_rem(&DAY);
    let (hours, seconds) = seconds.div_rem(&HOUR);
    let (minutes, seconds) = seconds.div_rem(&MINUTE);

    let mut sections = vec![];

    for (var, name) in [days, hours, minutes, seconds]
        .iter()
        .zip(["days", "hours", "minutes", "seconds"].iter())
    {
        if *var > 0 {
            sections.push(format!("{} {}", var, name));
        }
    }

    sections.join(", ")
}

async fn create_webhook(
    ctx: impl CacheHttp,
    channel: GuildChannel,
    name: impl Display,
) -> SerenityResult<Webhook> {
    channel
        .create_webhook_with_avatar(
            ctx.http(),
            name,
            (
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/",
                    env!(
                        "WEBHOOK_AVATAR",
                        "WEBHOOK_AVATAR not provided for compilation"
                    )
                )) as &[u8],
                env!("WEBHOOK_AVATAR"),
            ),
        )
        .await
}

#[command]
#[supports_dm(false)]
#[permission_level(Restricted)]
async fn pause(ctx: &Context, msg: &Message, args: String) {
    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let language = UserData::language_of(&msg.author, &pool).await;
    let timezone = UserData::timezone_of(&msg.author, &pool).await;

    let mut channel = ChannelData::from_channel(msg.channel(&ctx).await.unwrap(), &pool)
        .await
        .unwrap();

    if args.is_empty() {
        channel.paused = !channel.paused;
        channel.paused_until = None;

        channel.commit_changes(&pool).await;

        if channel.paused {
            let _ = msg
                .channel_id
                .say(&ctx, lm.get(&language, "pause/paused_indefinite"))
                .await;
        } else {
            let _ = msg
                .channel_id
                .say(&ctx, lm.get(&language, "pause/unpaused"))
                .await;
        }
    } else {
        let parser = TimeParser::new(&args, timezone);
        let pause_until = parser.timestamp();

        match pause_until {
            Ok(timestamp) => {
                let dt = NaiveDateTime::from_timestamp(timestamp, 0);

                channel.paused = true;
                channel.paused_until = Some(dt);

                channel.commit_changes(&pool).await;

                let content = lm.get(&language, "pause/paused_until").replace(
                    "{}",
                    &timezone
                        .timestamp(timestamp, 0)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                );

                let _ = msg.channel_id.say(&ctx, content).await;
            }

            Err(_) => {
                let _ = msg
                    .channel_id
                    .say(&ctx, lm.get(&language, "pause/invalid_time"))
                    .await;
            }
        }
    }
}

#[command]
#[permission_level(Restricted)]
async fn offset(ctx: &Context, msg: &Message, args: String) {
    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();

    if args.is_empty() {
        let prefix = GuildData::prefix_from_id(msg.guild_id, &pool).await;
        let desc = lm.get(&user_data.language, "help/offset");

        let _ = msg
            .channel_id
            .send_message(ctx, |m| {
                m.embed(move |e| {
                    e.title("Offset Help")
                        .description(desc.replace("{prefix}", &prefix))
                        .color(*THEME_COLOR)
                })
            })
            .await;
    } else {
        let parser = TimeParser::new(&args, user_data.timezone());

        if let Ok(displacement) = parser.displacement() {
            if let Some(guild) = msg.guild(&ctx).await {
                let guild_data = GuildData::from_guild(guild, &pool).await.unwrap();

                sqlx::query!(
                    "
UPDATE reminders
    INNER JOIN `channels`
        ON `channels`.id = reminders.channel_id
    SET
        reminders.`time` = reminders.`time` + ?
    WHERE channels.guild_id = ?
                    ",
                    displacement,
                    guild_data.id
                )
                .execute(&pool)
                .await
                .unwrap();
            } else {
                sqlx::query!(
                    "
UPDATE reminders SET `time` = `time` + ? WHERE reminders.channel_id = ?
                    ",
                    displacement,
                    user_data.dm_channel
                )
                .execute(&pool)
                .await
                .unwrap();
            }

            let response = lm.get(&user_data.language, "offset/success").replacen(
                "{}",
                &displacement.to_string(),
                1,
            );

            let _ = msg.channel_id.say(&ctx, response).await;
        } else {
            let _ = msg
                .channel_id
                .say(&ctx, lm.get(&user_data.language, "offset/invalid_time"))
                .await;
        }
    }
}

#[command]
#[permission_level(Restricted)]
async fn nudge(ctx: &Context, msg: &Message, args: String) {
    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let language = UserData::language_of(&msg.author, &pool).await;
    let timezone = UserData::timezone_of(&msg.author, &pool).await;

    let mut channel = ChannelData::from_channel(msg.channel(&ctx).await.unwrap(), &pool)
        .await
        .unwrap();

    if args.is_empty() {
        let content = lm
            .get(&language, "nudge/no_argument")
            .replace("{nudge}", &format!("{}s", &channel.nudge.to_string()));

        let _ = msg.channel_id.say(&ctx, content).await;
    } else {
        let parser = TimeParser::new(&args, timezone);
        let nudge_time = parser.displacement();

        match nudge_time {
            Ok(displacement) => {
                if displacement < i16::MIN as i64 || displacement > i16::MAX as i64 {
                    let _ = msg
                        .channel_id
                        .say(&ctx, lm.get(&language, "nudge/invalid_time"))
                        .await;
                } else {
                    channel.nudge = displacement as i16;

                    channel.commit_changes(&pool).await;

                    let response = lm.get(&language, "nudge/success").replacen(
                        "{}",
                        &displacement.to_string(),
                        1,
                    );

                    let _ = msg.channel_id.say(&ctx, response).await;
                }
            }

            Err(_) => {
                let _ = msg
                    .channel_id
                    .say(&ctx, lm.get(&language, "nudge/invalid_time"))
                    .await;
            }
        }
    }
}

enum TimeDisplayType {
    Absolute,
    Relative,
}

enum Selection<T> {
    None,
    Single(T),
    All,
}

struct LookFlags {
    pub limit: u16,
    pub show_disabled: bool,
    pub channel_id: Selection<u64>,
    time_display: TimeDisplayType,
}

impl Default for LookFlags {
    fn default() -> Self {
        Self {
            limit: u16::MAX,
            show_disabled: true,
            channel_id: Selection::None,
            time_display: TimeDisplayType::Relative,
        }
    }
}

impl LookFlags {
    fn from_string(args: &str) -> Self {
        let mut new_flags: Self = Default::default();

        for arg in args.split(' ') {
            match arg {
                "enabled" => {
                    new_flags.show_disabled = false;
                }

                "time" => {
                    new_flags.time_display = TimeDisplayType::Absolute;
                }

                "all" => {
                    new_flags.channel_id = Selection::All;
                }

                param => {
                    if let Ok(val) = param.parse::<u16>() {
                        new_flags.limit = val;
                    } else {
                        new_flags.channel_id = REGEX_CHANNEL
                            .captures(&args)
                            .map(|cap| cap.get(1))
                            .flatten()
                            .map(|c| c.as_str().parse::<u64>().unwrap())
                            .map_or(Selection::None, Selection::Single);
                    }
                }
            }
        }

        new_flags
    }
}

struct LookReminder {
    id: u32,
    time: u32,
    channel: u64,
    content: String,
    description: Option<String>,
}

impl LookReminder {
    fn display_content(&self) -> String {
        if self.content.len() > 0 {
            self.content.clone()
        } else {
            self.description.clone().unwrap_or(String::from(""))
        }
    }
}

#[command("look")]
#[permission_level(Managed)]
async fn look(ctx: &Context, msg: &Message, args: String) {
    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let language = UserData::language_of(&msg.author, &pool).await;
    let timezone = UserData::timezone_of(&msg.author, &pool).await;

    let flags = LookFlags::from_string(&args);

    let enabled = if flags.show_disabled { "0,1" } else { "1" };

    let reminders = if let Some(guild_id) = msg.guild_id.map(|f| f.as_u64().to_owned()) {
        let channel_id_opt = match flags.channel_id {
            Selection::None => Some(msg.channel_id.as_u64().to_owned()),
            Selection::Single(id) => Some(id),
            Selection::All => None,
        };

        if let Some(channel_id) = channel_id_opt {
            sqlx::query_as!(
                LookReminder,
                "
SELECT
    reminders.id, reminders.time, channels.channel, messages.content, embeds.description
FROM
    reminders
INNER JOIN
    channels
ON
    reminders.channel_id = channels.id
INNER JOIN
    messages
ON
    messages.id = reminders.message_id
LEFT JOIN
    embeds
ON
    embeds.id = messages.embed_id
WHERE
    channels.guild_id = (SELECT id FROM guilds WHERE guild = ?) AND
    channels.channel = ? AND
    FIND_IN_SET(reminders.enabled, ?)
ORDER BY
    reminders.time
LIMIT
    ?
            ",
                guild_id,
                channel_id,
                enabled,
                flags.limit
            )
            .fetch_all(&pool)
            .await
        } else {
            sqlx::query_as!(
                LookReminder,
                "
SELECT
    reminders.id, reminders.time, channels.channel, messages.content, embeds.description
FROM
    reminders
INNER JOIN
    channels
ON
    reminders.channel_id = channels.id
INNER JOIN
    messages
ON
    messages.id = reminders.message_id
LEFT JOIN
    embeds
ON
    embeds.id = messages.embed_id
WHERE
    channels.guild_id = (SELECT id FROM guilds WHERE guild = ?) AND
    FIND_IN_SET(reminders.enabled, ?)
ORDER BY
    reminders.time
LIMIT
    ?
            ",
                guild_id,
                enabled,
                flags.limit
            )
            .fetch_all(&pool)
            .await
        }
    } else {
        sqlx::query_as_unchecked!(
            LookReminder,
            "
SELECT
    reminders.id, reminders.time, channels.channel, messages.content, embeds.description
FROM
    reminders
LEFT OUTER JOIN
    channels
ON
    channels.id = reminders.channel_id
INNER JOIN
    messages
ON
    messages.id = reminders.message_id
LEFT JOIN
    embeds
ON
    embeds.id = messages.embed_id
WHERE
    channels.channel = ? AND
    FIND_IN_SET(reminders.enabled, ?)
ORDER BY
    reminders.time
LIMIT
    ?
            ",
            msg.channel_id.as_u64(),
            enabled,
            flags.limit
        )
        .fetch_all(&pool)
        .await
    }
    .unwrap();

    if reminders.is_empty() {
        let _ = msg
            .channel_id
            .say(&ctx, lm.get(&language, "look/no_reminders"))
            .await;
    } else {
        let inter = lm.get(&language, "look/inter");

        let display = reminders.iter().map(|reminder| {
            let time_display = match flags.time_display {
                TimeDisplayType::Absolute => timezone
                    .timestamp(reminder.time as i64, 0)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
                TimeDisplayType::Relative => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs();

                    longhand_displacement((reminder.time as u64).checked_sub(now).unwrap_or(1))
                }
            };

            format!(
                "'{}' *{}* **{}**",
                reminder.display_content(),
                &inter,
                time_display
            )
        });

        let _ = msg.channel_id.say_lines(&ctx, display).await;
    }
}

#[command("del")]
#[permission_level(Managed)]
async fn delete(ctx: &Context, msg: &Message, _args: String) {
    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();

    let _ = msg
        .channel_id
        .say(&ctx, lm.get(&user_data.language, "del/listing"))
        .await;

    let reminders = if let Some(guild_id) = msg.guild_id.map(|f| f.as_u64().to_owned()) {
        sqlx::query_as_unchecked!(
            LookReminder,
            "
SELECT
    reminders.id, reminders.time, channels.channel, messages.content, embeds.description
FROM
    reminders
LEFT OUTER JOIN
    channels
ON
    channels.id = reminders.channel_id
INNER JOIN
    messages
ON
    messages.id = reminders.message_id
LEFT JOIN
    embeds
ON
    embeds.id = messages.embed_id
WHERE
    channels.guild_id = (SELECT id FROM guilds WHERE guild = ?)
            ",
            guild_id
        )
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query_as!(
            LookReminder,
            "
SELECT
    reminders.id, reminders.time, channels.channel, messages.content, embeds.description
FROM
    reminders
INNER JOIN
    messages
ON
    reminders.message_id = messages.id
LEFT JOIN
    embeds
ON
    embeds.id = messages.embed_id
INNER JOIN
    channels
ON
    channels.id = reminders.channel_id
WHERE
    channels.channel = ?
            ",
            msg.channel_id.as_u64()
        )
        .fetch_all(&pool)
        .await
    }
    .unwrap();

    let mut reminder_ids: Vec<u32> = vec![];

    let enumerated_reminders = reminders.iter().enumerate().map(|(count, reminder)| {
        reminder_ids.push(reminder.id);
        let time = user_data.timezone().timestamp(reminder.time as i64, 0);

        format!(
            "**{}**: '{}' *<#{}>* at {}",
            count + 1,
            reminder.display_content(),
            reminder.channel,
            time.format("%Y-%m-%d %H:%M:%S")
        )
    });

    let _ = msg.channel_id.say_lines(&ctx, enumerated_reminders).await;
    let _ = msg
        .channel_id
        .say(&ctx, lm.get(&user_data.language, "del/listed"))
        .await;

    let reply = msg
        .channel_id
        .await_reply(&ctx)
        .author_id(msg.author.id)
        .channel_id(msg.channel_id)
        .await;

    if let Some(content) = reply.map(|m| m.content.replace(",", " ")) {
        let parts = content
            .split(' ')
            .filter(|i| !i.is_empty())
            .collect::<Vec<&str>>();

        let valid_parts = parts
            .iter()
            .filter_map(|i| {
                i.parse::<usize>()
                    .ok()
                    .filter(|val| val > &0)
                    .map(|val| reminder_ids.get(val - 1))
                    .flatten()
            })
            .map(|item| item.to_string())
            .collect::<Vec<String>>();

        if parts.len() == valid_parts.len() {
            let joined = valid_parts.join(",");

            let count_row = sqlx::query!(
                "
SELECT COUNT(1) AS count FROM reminders WHERE FIND_IN_SET(id, ?)
                ",
                joined
            )
            .fetch_one(&pool)
            .await
            .unwrap();

            sqlx::query!(
                "
DELETE FROM reminders WHERE FIND_IN_SET(id, ?)
                ",
                joined
            )
            .execute(&pool)
            .await
            .unwrap();

            if let Some(guild_id) = msg.guild_id {
                let _ = sqlx::query!(
                    "
INSERT INTO events (event_name, bulk_count, guild_id, user_id) VALUES ('delete', ?, ?, ?)
                    ",
                    count_row.count,
                    guild_id.as_u64(),
                    user_data.id
                )
                .execute(&pool)
                .await;
            }

            let content = lm.get(&user_data.language, "del/count").replacen(
                "{}",
                &count_row.count.to_string(),
                1,
            );

            let _ = msg.channel_id.say(&ctx, content).await;
        } else {
            let content = lm
                .get(&user_data.language, "del/count")
                .replacen("{}", "0", 1);

            let _ = msg.channel_id.say(&ctx, content).await;
        }
    }
}

#[command("timer")]
#[permission_level(Managed)]
async fn timer(ctx: &Context, msg: &Message, args: String) {
    fn time_difference(start_time: NaiveDateTime) -> String {
        let unix_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let now = NaiveDateTime::from_timestamp(unix_time, 0);

        let delta = (now - start_time).num_seconds();

        let (minutes, seconds) = delta.div_rem(&60);
        let (hours, minutes) = minutes.div_rem(&60);

        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    }

    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let language = UserData::language_of(&msg.author, &pool).await;

    let mut args_iter = args.splitn(2, ' ');

    let owner = msg
        .guild_id
        .map(|g| g.as_u64().to_owned())
        .unwrap_or_else(|| msg.author.id.as_u64().to_owned());

    match args_iter.next() {
        Some("list") => {
            let timers = Timer::from_owner(owner, &pool).await;

            let _ = msg
                .channel_id
                .send_message(&ctx, |m| {
                    m.embed(|e| {
                        e.fields(timers.iter().map(|timer| {
                            (
                                &timer.name,
                                format!("⏳ `{}`", time_difference(timer.start_time)),
                                false,
                            )
                        }))
                    })
                })
                .await;
        }

        Some("start") => {
            let count = Timer::count_from_owner(owner, &pool).await;

            if count >= 25 {
                let _ = msg
                    .channel_id
                    .say(&ctx, lm.get(&language, "timer/limit"))
                    .await;
            } else {
                let name = args_iter
                    .next()
                    .map(|s| s.to_string())
                    .unwrap_or(format!("New timer #{}", count + 1));

                Timer::create(&name, owner, &pool).await;

                let _ = msg
                    .channel_id
                    .say(&ctx, lm.get(&language, "timer/success"))
                    .await;
            }
        }

        Some("delete") => {
            if let Some(name) = args_iter.next() {
                let exists = sqlx::query!(
                    "
SELECT 1 as _r FROM timers WHERE owner = ? AND name = ?
                    ",
                    owner,
                    name
                )
                .fetch_one(&pool)
                .await;

                if exists.is_ok() {
                    sqlx::query!(
                        "
DELETE FROM timers WHERE owner = ? AND name = ?
                        ",
                        owner,
                        name
                    )
                    .execute(&pool)
                    .await
                    .unwrap();

                    let _ = msg
                        .channel_id
                        .say(&ctx, lm.get(&language, "timer/deleted"))
                        .await;
                } else {
                    let _ = msg
                        .channel_id
                        .say(&ctx, lm.get(&language, "timer/not_found"))
                        .await;
                }
            } else {
                let _ = msg
                    .channel_id
                    .say(&ctx, lm.get(&language, "timer/help"))
                    .await;
            }
        }

        _ => {
            let desc = lm.get(&language, "help/timer");
            let prefix = GuildData::prefix_from_id(msg.guild_id, &pool).await;

            let _ = msg
                .channel_id
                .send_message(ctx, |m| {
                    m.embed(move |e| {
                        e.title("Timer Help")
                            .description(desc.replace("{prefix}", &prefix))
                            .color(*THEME_COLOR)
                    })
                })
                .await;
        }
    }
}

#[derive(PartialEq)]
enum RemindCommand {
    Remind,
    Interval,
}

enum ReminderScope {
    User(u64),
    Channel(u64),
}

impl Mentionable for ReminderScope {
    fn mention(&self) -> String {
        match self {
            Self::User(id) => format!("<@{}>", id),
            Self::Channel(id) => format!("<#{}>", id),
        }
    }
}

#[derive(PartialEq, Eq, Hash, Debug)]
enum ReminderError {
    LongTime,
    LongInterval,
    PastTime,
    ShortInterval,
    InvalidTag,
    NotEnoughArgs,
    InvalidTime,
    NeedSubscription,
    DiscordError,
}

impl std::fmt::Display for ReminderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_response())
    }
}

impl std::error::Error for ReminderError {}

trait ToResponse {
    fn to_response(&self) -> &'static str;

    fn to_response_natural(&self) -> &'static str;
}

impl ToResponse for ReminderError {
    fn to_response(&self) -> &'static str {
        match self {
            Self::LongTime => "remind/long_time",
            Self::LongInterval => "interval/long_interval",
            Self::PastTime => "remind/past_time",
            Self::ShortInterval => "interval/short_interval",
            Self::InvalidTag => "remind/invalid_tag",
            Self::NotEnoughArgs => "remind/no_argument",
            Self::InvalidTime => "remind/invalid_time",
            Self::NeedSubscription => "interval/donor",
            Self::DiscordError => "remind/no_webhook",
        }
    }

    fn to_response_natural(&self) -> &'static str {
        match self {
            Self::LongTime => "natural/long_time",
            Self::InvalidTime => "natural/invalid_time",
            _ => self.to_response(),
        }
    }
}

impl<T> ToResponse for Result<T, ReminderError> {
    fn to_response(&self) -> &'static str {
        match self {
            Ok(_) => "remind/success",

            Err(reminder_error) => reminder_error.to_response(),
        }
    }

    fn to_response_natural(&self) -> &'static str {
        match self {
            Ok(_) => "remind/success",

            Err(reminder_error) => reminder_error.to_response_natural(),
        }
    }
}

fn generate_uid() -> String {
    let mut generator: OsRng = Default::default();

    (0..64)
        .map(|_| {
            CHARACTERS
                .chars()
                .choose(&mut generator)
                .unwrap()
                .to_owned()
                .to_string()
        })
        .collect::<Vec<String>>()
        .join("")
}

#[command("remind")]
#[permission_level(Managed)]
async fn remind(ctx: &Context, msg: &Message, args: String) {
    remind_command(ctx, msg, args, RemindCommand::Remind).await;
}

#[command("interval")]
#[permission_level(Managed)]
async fn interval(ctx: &Context, msg: &Message, args: String) {
    if check_subscription_on_message(&ctx, msg).await {
        remind_command(ctx, msg, args, RemindCommand::Interval).await;
    } else {
        let _ = msg.channel_id.say(&ctx, "interval/donor").await;
    }
}

async fn remind_command(ctx: &Context, msg: &Message, args: String, command: RemindCommand) {
    fn parse_mention_list(mentions: &str) -> Vec<ReminderScope> {
        REGEX_CHANNEL_USER
            .captures_iter(mentions)
            .map(|i| {
                let pref = i.get(1).unwrap().as_str();
                let id = i.get(2).unwrap().as_str().parse::<u64>().unwrap();

                if pref == "#" {
                    ReminderScope::Channel(id)
                } else {
                    ReminderScope::User(id)
                }
            })
            .collect::<Vec<ReminderScope>>()
    }

    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();

    let captures = match command {
        RemindCommand::Remind => REGEX_REMIND_COMMAND.captures(&args),

        RemindCommand::Interval => REGEX_INTERVAL_COMMAND.captures(&args),
    };

    match captures {
        Some(captures) => {
            let parsed = parse_mention_list(captures.name("mentions").unwrap().as_str());

            let scopes = if parsed.len() == 0 {
                vec![ReminderScope::Channel(msg.channel_id.into())]
            } else {
                parsed
            };

            let time_parser = TimeParser::new(
                captures.name("time").unwrap().as_str(),
                user_data.timezone(),
            );

            let interval_parser = captures
                .name("interval")
                .map(|mat| TimeParser::new(mat.as_str(), user_data.timezone()))
                // todo remove unwrap below
                .map(|parser| parser.displacement().unwrap());

            let content = captures.name("content").map(|mat| mat.as_str()).unwrap();

            let mut ok_locations = vec![];
            let mut err_locations = vec![];
            let mut err_types = HashSet::new();

            for scope in scopes {
                let res = create_reminder(
                    &ctx,
                    &pool,
                    msg.author.id,
                    msg.guild_id,
                    &scope,
                    &time_parser,
                    interval_parser,
                    content,
                )
                .await;

                if let Err(e) = res {
                    err_locations.push(scope);
                    err_types.insert(e);
                } else {
                    ok_locations.push(scope);
                }
            }

            let success_part = match ok_locations.len() {
                0 => "".to_string(),
                1 => lm
                    .get(&user_data.language, "remind/success")
                    .replace("{location}", &ok_locations[0].mention())
                    .replace(
                        "{offset}",
                        &shorthand_displacement(time_parser.displacement().unwrap() as u64),
                    ),
                n => lm
                    .get(&user_data.language, "remind/success_bulk")
                    .replace("{number}", &n.to_string())
                    .replace(
                        "{location}",
                        &ok_locations
                            .iter()
                            .map(|l| l.mention())
                            .collect::<Vec<String>>()
                            .join(", "),
                    )
                    .replace(
                        "{offset}",
                        &shorthand_displacement(time_parser.displacement().unwrap() as u64),
                    ),
            };

            let error_part = format!(
                "{}\n{}",
                match err_locations.len() {
                    0 => "".to_string(),
                    1 => lm
                        .get(&user_data.language, "remind/issue")
                        .replace("{location}", &err_locations[0].mention()),
                    n => lm
                        .get(&user_data.language, "remind/issue_bulk")
                        .replace("{number}", &n.to_string())
                        .replace(
                            "{location}",
                            &err_locations
                                .iter()
                                .map(|l| l.mention())
                                .collect::<Vec<String>>()
                                .join(", "),
                        ),
                },
                err_types
                    .iter()
                    .map(|err| lm.get(&user_data.language, err.to_response()))
                    .collect::<Vec<&str>>()
                    .join("\n")
            );

            let _ = msg
                .channel_id
                .send_message(&ctx, |m| {
                    m.embed(|e| {
                        e.title(
                            lm.get(&user_data.language, "remind/title")
                                .replace("{number}", &ok_locations.len().to_string()),
                        )
                        .description(format!("{}\n\n{}", success_part, error_part))
                        .color(*THEME_COLOR)
                    })
                })
                .await;
        }

        None => {
            let desc = lm.get(
                &user_data.language,
                match command {
                    RemindCommand::Remind => "help/remind",

                    RemindCommand::Interval => "help/interval",
                },
            );

            let title = match command {
                RemindCommand::Remind => "Remind Help",
                RemindCommand::Interval => "Interval Help",
            };

            let prefix = GuildData::prefix_from_id(msg.guild_id, &pool).await;

            let _ = msg
                .channel_id
                .send_message(ctx, |m| {
                    m.embed(move |e| {
                        e.title(title)
                            .description(desc.replace("{prefix}", &prefix))
                            .color(*THEME_COLOR)
                    })
                })
                .await;
        }
    }
}

#[command("natural")]
#[permission_level(Managed)]
async fn natural(ctx: &Context, msg: &Message, args: String) {
    let pool;
    let lm;

    {
        let data = ctx.data.read().await;

        pool = data
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        lm = data.get::<LanguageManager>().cloned().unwrap();
    }

    let now = SystemTime::now();
    let since_epoch = now
        .duration_since(UNIX_EPOCH)
        .expect("Time calculated as going backwards. Very bad");

    let user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();

    let send_str = lm.get(&user_data.language, "natural/send");
    let to_str = lm.get(&user_data.language, "natural/to");
    let every_str = lm.get(&user_data.language, "natural/every");

    let mut args_iter = args.splitn(2, &send_str);

    let (time_crop_opt, msg_crop_opt) = (args_iter.next(), args_iter.next().map(|m| m.trim()));

    if let (Some(time_crop), Some(msg_crop)) = (time_crop_opt, msg_crop_opt) {
        let python_call = Command::new(&*PYTHON_LOCATION)
            .arg("-c")
            .arg(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/dp.py")))
            .arg(time_crop)
            .arg(&user_data.timezone)
            .arg(&*LOCAL_TIMEZONE)
            .output()
            .await;

        if let Some(timestamp) = python_call
            .ok()
            .map(|inner| {
                if inner.status.success() {
                    Some(from_utf8(&*inner.stdout).unwrap().parse::<i64>().unwrap())
                } else {
                    None
                }
            })
            .flatten()
        {
            let mut location_ids = vec![ReminderScope::Channel(msg.channel_id.as_u64().to_owned())];
            let mut content = msg_crop;
            let mut interval = None;

            if msg.guild_id.is_some() {
                let re_match = RegexBuilder::new(&format!(r#"(?:\s*)(?P<msg>.*) {} (?P<mentions>((?:<@\d+>)|(?:<@!\d+>)|(?:<#\d+>)|(?:\s+))+)$"#, to_str))
                    .dot_matches_new_line(true)
                    .build()
                    .unwrap()
                    .captures(msg_crop);

                if let Some(captures) = re_match {
                    content = captures.name("msg").unwrap().as_str();

                    let mentions = captures.name("mentions").unwrap().as_str();

                    location_ids = REGEX_CHANNEL_USER
                        .captures_iter(mentions)
                        .map(|i| {
                            let pref = i.get(1).unwrap().as_str();
                            let id = i.get(2).unwrap().as_str().parse::<u64>().unwrap();

                            if pref == "#" {
                                ReminderScope::Channel(id)
                            } else {
                                ReminderScope::User(id)
                            }
                        })
                        .collect::<Vec<ReminderScope>>();
                }
            }

            if check_subscription_on_message(&ctx, &msg).await {
                let re_match =
                    RegexBuilder::new(&format!(r#"(?P<msg>.*) {} (?P<interval>.*)$"#, every_str))
                        .dot_matches_new_line(true)
                        .build()
                        .unwrap()
                        .captures(content);

                if let Some(captures) = re_match {
                    content = captures.name("msg").unwrap().as_str();

                    let interval_str = captures.name("interval").unwrap().as_str();

                    let python_call = Command::new(&*PYTHON_LOCATION)
                        .arg("-c")
                        .arg(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/dp.py")))
                        .arg(&format!("1 {}", interval_str))
                        .arg(&*LOCAL_TIMEZONE)
                        .arg(&*LOCAL_TIMEZONE)
                        .output()
                        .await;

                    interval = python_call
                        .ok()
                        .map(|inner| {
                            if inner.status.success() {
                                Some(
                                    from_utf8(&*inner.stdout).unwrap().parse::<i64>().unwrap()
                                        - since_epoch.as_secs() as i64,
                                )
                            } else {
                                None
                            }
                        })
                        .flatten();
                }
            }

            if location_ids.len() == 1 {
                let location_id = location_ids.get(0).unwrap();

                let res = create_reminder(
                    &ctx,
                    &pool,
                    msg.author.id,
                    msg.guild_id,
                    &location_id,
                    timestamp,
                    interval,
                    &content,
                )
                .await;

                let offset = timestamp as u64 - since_epoch.as_secs();

                let str_response = lm
                    .get(&user_data.language, &res.to_response_natural())
                    .replace(
                        "{prefix}",
                        &GuildData::prefix_from_id(msg.guild_id, &pool).await,
                    )
                    .replacen("{location}", &location_id.mention(), 1)
                    .replacen("{offset}", &shorthand_displacement(offset), 1)
                    .replacen("{min_interval}", &MIN_INTERVAL.to_string(), 1)
                    .replacen("{max_time}", &MAX_TIME.to_string(), 1);

                let _ = msg.channel_id.say(&ctx, &str_response).await;
            } else {
                let mut ok_count = 0_u8;

                for location in location_ids {
                    let res = create_reminder(
                        &ctx,
                        &pool,
                        msg.author.id,
                        msg.guild_id,
                        &location,
                        timestamp,
                        interval,
                        &content,
                    )
                    .await;

                    if res.is_ok() {
                        ok_count += 1;
                    }
                }

                let content = lm
                    .get(&user_data.language, "natural/bulk_set")
                    .replace("{}", &ok_count.to_string());

                let _ = msg.channel_id.say(&ctx, content).await;
            }
        } else {
            let _ = msg
                .channel_id
                .say(&ctx, "DEV ERROR: Failed to invoke Python")
                .await;
        }
    } else {
        let prefix = GuildData::prefix_from_id(msg.guild_id, &pool).await;

        let resp = lm
            .get(&user_data.language, "natural/no_argument")
            .replace("{prefix}", &prefix);

        let _ = msg
            .channel_id
            .send_message(&ctx, |m| m.embed(|e| e.description(resp)))
            .await;
    }
}

async fn create_reminder<
    'a,
    U: Into<u64>,
    T: TryInto<i64>,
    S: ToString + Type<MySql> + Encode<'a, MySql>,
>(
    ctx: impl CacheHttp,
    pool: &MySqlPool,
    user_id: U,
    guild_id: Option<GuildId>,
    scope_id: &ReminderScope,
    time_parser: T,
    interval: Option<i64>,
    content: S,
) -> Result<(), ReminderError> {
    let user_id = user_id.into();

    let mut content_string = content.to_string();

    // substitution filters
    content_string = content_string.replace("<<everyone>>", "@everyone");
    content_string = content_string.replace("<<here>>", "@here");
    content_string = REGEX_CONTENT_SUBSTITUTION
        .replace(&content_string, "<@$1>")
        .to_string();

    let mut nudge = 0;

    let db_channel_id = match scope_id {
        ReminderScope::User(user_id) => {
            let user = UserId(*user_id).to_user(&ctx).await.unwrap();

            let user_data = UserData::from_user(&user, &ctx, &pool).await.unwrap();

            user_data.dm_channel
        }

        ReminderScope::Channel(channel_id) => {
            let channel = ChannelId(*channel_id).to_channel(&ctx).await.unwrap();

            if channel.clone().guild().map(|gc| gc.guild_id) != guild_id {
                return Err(ReminderError::InvalidTag);
            }

            let mut channel_data = ChannelData::from_channel(channel.clone(), &pool)
                .await
                .unwrap();
            nudge = channel_data.nudge;

            if let Some(guild_channel) = channel.guild() {
                if channel_data.webhook_token.is_none() || channel_data.webhook_id.is_none() {
                    if let Ok(webhook) = create_webhook(&ctx, guild_channel, "Reminder").await {
                        channel_data.webhook_id = Some(webhook.id.as_u64().to_owned());
                        channel_data.webhook_token = Some(webhook.token);

                        channel_data.commit_changes(&pool).await;
                    } else {
                        return Err(ReminderError::DiscordError);
                    }
                }
            }

            channel_data.id
        }
    };

    // validate time, channel, content
    if content_string.is_empty() {
        Err(ReminderError::NotEnoughArgs)
    } else if interval.map_or(false, |inner| inner < *MIN_INTERVAL) {
        Err(ReminderError::ShortInterval)
    } else if interval.map_or(false, |inner| inner > *MAX_TIME) {
        Err(ReminderError::LongInterval)
    } else {
        match time_parser.try_into() {
            Ok(time_pre) => {
                let time = time_pre + nudge as i64;

                let unix_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;

                if time >= unix_time - 10 {
                    if time > unix_time + *MAX_TIME {
                        Err(ReminderError::LongTime)
                    } else {
                        sqlx::query!(
                            "
INSERT INTO messages (content) VALUES (?)
                            ",
                            content_string
                        )
                        .execute(&pool.clone())
                        .await
                        .unwrap();

                        sqlx::query!(
                            "
INSERT INTO reminders (uid, message_id, channel_id, time, `interval`, method, set_by) VALUES
    (?,
    (SELECT id FROM messages WHERE content = ? ORDER BY id DESC LIMIT 1),
    ?, ?, ?, 'remind',
    (SELECT id FROM users WHERE user = ? LIMIT 1))
                            ",
                            generate_uid(),
                            content_string,
                            db_channel_id,
                            time as u32,
                            interval,
                            user_id
                        )
                        .execute(pool)
                        .await
                        .unwrap();

                        Ok(())
                    }
                } else if time < 0 {
                    // case required for if python returns -1
                    Err(ReminderError::InvalidTime)
                } else {
                    Err(ReminderError::PastTime)
                }
            }

            Err(_) => Err(ReminderError::InvalidTime),
        }
    }
}
