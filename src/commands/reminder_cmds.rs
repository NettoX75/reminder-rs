use regex_command_attr::command;

use serenity::{
    client::Context,
    model::{
        channel::{
            Message,
        },
    },
    framework::standard::CommandResult,
};

use crate::{
    models::{
        ChannelData,
        GuildData,
        UserData,
        Reminder,
        Timer,
    },
    SQLPool,
    time_parser::TimeParser,
};

use chrono::NaiveDateTime;

use num_integer::Integer;

use std::{
    default::Default,
    time::{
        SystemTime,
        UNIX_EPOCH,
    }
};

use regex::Regex;

lazy_static! {
    static ref REGEX_CHANNEL: Regex = Regex::new(r#"^\s*<#(\d+)>\s*$"#).unwrap();
}


#[command]
#[supports_dm(false)]
#[permission_level(Restricted)]
async fn pause(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_id(&msg.author, &ctx, &pool).await.unwrap();
    let mut channel = ChannelData::from_channel(msg.channel(&ctx).await.unwrap(), &pool).await.unwrap();

    if args.len() == 0 {
        channel.paused = !channel.paused;
        channel.paused_until = None;

        channel.commit_changes(&pool).await;

        if channel.paused {
            let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "pause/paused_indefinite").await).await;
        }
        else {
            let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "pause/unpaused").await).await;
        }
    }
    else {
        let parser = TimeParser::new(args, user_data.timezone.parse().unwrap());
        let pause_until = parser.timestamp();

        match pause_until {
            Ok(timestamp) => {
                channel.paused = true;
                channel.paused_until = Some(NaiveDateTime::from_timestamp(timestamp, 0));

                channel.commit_changes(&pool).await;

                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "pause/paused_until").await).await;
            },

            Err(_) => {
                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "pause/invalid_time").await).await;
            },
        }
    }

    Ok(())
}

#[command]
#[permission_level(Restricted)]
async fn offset(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_id(&msg.author, &ctx, &pool).await.unwrap();

    if args.len() == 0 {
        let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "offset/help").await).await;
    }
    else {
        let parser = TimeParser::new(args, user_data.timezone());

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
                    ", displacement, guild_data.id)
                    .execute(&pool)
                    .await
                    .unwrap();
            } else {
                sqlx::query!(
                    "
UPDATE reminders SET `time` = `time` + ? WHERE reminders.channel_id = ?
                    ", displacement, user_data.dm_channel)
                    .execute(&pool)
                    .await
                    .unwrap();
            }

            let response = user_data.response(&pool, "offset/success").await.replacen("{}", &displacement.to_string(), 1);

            let _ = msg.channel_id.say(&ctx, response).await;
        } else {
            let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "offset/invalid_time").await).await;
        }
    }

    Ok(())
}

#[command]
#[permission_level(Restricted)]
async fn nudge(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_id(&msg.author, &ctx, &pool).await.unwrap();
    let mut channel = ChannelData::from_channel(msg.channel(&ctx).await.unwrap(), &pool).await.unwrap();

    if args.len() == 0 {
        let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "nudge/invalid_time").await).await;
    }
    else {
        let parser = TimeParser::new(args, user_data.timezone.parse().unwrap());
        let nudge_time = parser.displacement();

        match nudge_time {
            Ok(displacement) => {
                if displacement < i16::MIN as i64 || displacement > i16::MAX as i64 {
                    let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "nudge/invalid_time").await).await;
                }
                else {
                    channel.nudge = displacement as i16;

                    channel.commit_changes(&pool).await;

                    let response = user_data.response(&pool, "nudge/success").await.replacen("{}", &displacement.to_string(), 1);

                    let _ = msg.channel_id.say(&ctx, response).await;
                }
            },

            Err(_) => {
                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "nudge/invalid_time").await).await;
            },
        }
    }

    Ok(())
}

enum TimeDisplayType {
    Absolute,
    Relative,
}

struct LookFlags {
    pub limit: u16,
    pub show_disabled: bool,
    pub channel_id: Option<u64>,
    time_display: TimeDisplayType,
}

impl Default for LookFlags {
    fn default() -> Self {
        Self {
            limit: u16::MAX,
            show_disabled: true,
            channel_id: None,
            time_display: TimeDisplayType::Relative
        }
    }
}

impl LookFlags {
    fn from_string(args: &str) -> Self {
        let mut new_flags: Self = Default::default();

        for arg in args.split(" ") {
            match arg {
                "enabled" => {
                    new_flags.show_disabled = false;
                },

                "time" => {
                    new_flags.time_display = TimeDisplayType::Absolute;
                },

                param => {
                    if let Ok(val) = param.parse::<u16>() {
                        new_flags.limit = val;
                    }
                    else {
                        new_flags.channel_id = REGEX_CHANNEL.captures(&args)
                            .map(|cap| cap.get(1))
                            .flatten()
                            .map(|c| c.as_str().parse::<u64>().unwrap());
                    }
                }
            }
        }

        new_flags
    }

    fn display_time(&self, timestamp: u64) -> String {

        String::from("")
    }
}

#[command]
#[permission_level(Managed)]
async fn look(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_id(&msg.author, &ctx, &pool).await.unwrap();

    let flags = LookFlags::from_string(&args);

    let enabled = if flags.show_disabled { "0,1" } else { "1" };

    let reminders = if let Some(guild_id) = msg.guild_id.map(|f| f.as_u64().to_owned()) {
        let channel_id = flags.channel_id.unwrap_or(msg.channel_id.as_u64().to_owned());

        sqlx::query_as!(Reminder,
            "
SELECT
    reminders.id, reminders.time, reminders.name, reminders.channel_id
FROM
    reminders
INNER JOIN
    channels
ON
    reminders.channel_id = channels.id
WHERE
    channels.guild_id = (SELECT id FROM guilds WHERE guild = ?) AND
    channels.channel = ? AND
    FIND_IN_SET(reminders.enabled, ?)
LIMIT
    ?
            ", guild_id, channel_id, enabled, flags.limit)
            .fetch_all(&pool)
            .await
    }
    else {
        sqlx::query_as!(Reminder,
            "
SELECT
    id, time, name, channel_id
FROM
    reminders
WHERE
    reminders.channel_id = (SELECT id FROM channels WHERE channel = ?) AND
    FIND_IN_SET(reminders.enabled, ?)
LIMIT
    ?
            ", msg.channel_id.as_u64(), enabled, flags.limit)
            .fetch_all(&pool)
            .await
    }.unwrap();

    if reminders.len() == 0 {
        let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "look/no_reminders").await).await;
    }
    else {
        let inter = user_data.response(&pool, "look/inter").await;

        let display = reminders
            .iter()
            .map(|reminder| format!("'{}' *{}* **{}**", reminder.name, &inter, reminder.time))
            .collect::<Vec<String>>().join("\n");

        let _ = msg.channel_id.say(&ctx, display).await;
    }

    Ok(())
}

#[command]
#[permission_level(Managed)]
async fn delete(ctx: &Context, msg: &Message, _args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_id(&msg.author, &ctx, &pool).await.unwrap();

    let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "del/listing").await).await;

    let reminders = if let Some(guild_id) = msg.guild_id.map(|f| f.as_u64().to_owned()) {
        sqlx::query_as!(Reminder,
            "
SELECT
    reminders.id, reminders.time, reminders.name, reminders.channel_id
FROM
    reminders
INNER JOIN
    channels
ON
    reminders.channel_id = channels.id
WHERE
    channels.guild_id = (SELECT id FROM guilds WHERE guild = ?)
            ", guild_id)
            .fetch_all(&pool)
            .await
    }
    else {
        sqlx::query_as!(Reminder,
            "
SELECT
    id, time, name, channel_id
FROM
    reminders
WHERE
    channel_id = (SELECT id FROM channels WHERE channel = ?)
            ", msg.channel_id.as_u64())
            .fetch_all(&pool)
            .await
    }.unwrap();

    let mut reminder_ids: Vec<u32> = vec![];

    let enumerated_reminders = reminders
        .iter()
        .enumerate()
        .map(|(count, reminder)| {
            reminder_ids.push(reminder.id);
            format!("**{}**: '{}' *{}* at {}", count + 1, reminder.name, reminder.channel_id, reminder.time)
        })
        .collect::<Vec<String>>()
        .join("\n");

    let _ = msg.channel_id.say(&ctx, enumerated_reminders).await;
    let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "del/listed").await).await;

    let reply = msg.channel_id.await_reply(&ctx)
        .filter_limit(1)
        .author_id(msg.author.id)
        .channel_id(msg.channel_id).await;

    if let Some(content) = reply.map(|m| m.content.replace(",", " ")) {
        let parts = content.split(" ").filter(|i| i.len() > 0).collect::<Vec<&str>>();

        let valid_parts = parts
            .iter()
            .filter_map(
                |i|
                    i.parse::<usize>()
                        .ok()
                        .map(
                            |val|
                                reminder_ids.get(val - 1)
                        )
                        .flatten()
            )
            .map(|item| item.to_string())
            .collect::<Vec<String>>();

        if parts.len() == valid_parts.len() {
            sqlx::query!(
                "
DELETE FROM reminders WHERE FIND_IN_SET(id, ?)
                ", valid_parts.join(","))
                .execute(&pool)
                .await
                .unwrap();

            // TODO add deletion events to event list

            let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "del/count").await).await;
        }
    }

    Ok(())
}

#[command]
#[permission_level(Managed)]
async fn timer(ctx: &Context, msg: &Message, args: String) -> CommandResult {
        fn time_difference(start_time: NaiveDateTime) -> String {
        let unix_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let now = NaiveDateTime::from_timestamp(unix_time, 0);

        let delta = (now - start_time).num_seconds();

        let (minutes, seconds) = delta.div_rem(&60);
        let (hours, minutes) = minutes.div_rem(&60);

        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    }

    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_id(&msg.author, &ctx, &pool).await.unwrap();

    let mut args_iter = args.splitn(2, " ");

    let owner = msg.guild_id.map(|g| g.as_u64().to_owned()).unwrap_or(msg.author.id.as_u64().to_owned());

    match args_iter.next() {
        Some("list") => {
            let timers = Timer::from_owner(owner, &pool).await;

            let _ = msg.channel_id.send_message(&ctx, |m| m
                .embed(|e| {
                    e.fields(timers.iter().map(|timer| (&timer.name, time_difference(timer.start_time), false)))
                })
            ).await;
        },

        Some("start") => {
            let count = Timer::count_from_owner(owner, &pool).await;

            if count >= 25 {
                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "timer/limit").await).await;
            }
            else {
                let name = args_iter.next().map(|s| s.to_string()).unwrap_or(format!("New timer #{}", count + 1));

                Timer::create(&name, owner, &pool).await;

                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "timer/success").await).await;
            }
        },

        Some("delete") => {
            if let Some(name) = args_iter.next() {
                let exists = sqlx::query!(
                    "
SELECT 1 as _r FROM timers WHERE owner = ? AND name = ?
                    ", owner, name)
                    .fetch_one(&pool)
                    .await;

                if exists.is_ok() {
                    sqlx::query!(
                        "
DELETE FROM timers WHERE owner = ? AND name = ?
                        ", owner, name)
                        .execute(&pool)
                        .await
                        .unwrap();

                    let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "timer/deleted").await).await;
                }
                else {
                    let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "timer/not_found").await).await;
                }
            }
            else {
                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "timer/help").await).await;
            }
        },

        _ => {
            let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "timer/help").await).await;
        }
    }

    Ok(())
}
