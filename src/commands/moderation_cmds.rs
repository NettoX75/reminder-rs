use regex_command_attr::command;

use serenity::{
    client::Context,
    model::{
        id::RoleId,
        channel::{
            Message,
        },
    },
    framework::{
        Framework,
        standard::CommandResult,
    },
};

use regex::Regex;

use chrono_tz::Tz;

use chrono::offset::Utc;

use inflector::Inflector;

use crate::{
    models::{
        ChannelData,
        UserData,
        GuildData,
    },
    SQLPool,
    FrameworkCtx,
    framework::SendIterator,
};

use std::iter;

lazy_static! {
    static ref REGEX_CHANNEL: Regex = Regex::new(r#"^\s*<#(\d+)>\s*$"#).unwrap();

    static ref REGEX_ROLE: Regex = Regex::new(r#"<@&([0-9]+)>"#).unwrap();

    static ref REGEX_COMMANDS: Regex = Regex::new(r#"([a-z]+)"#).unwrap();

    static ref REGEX_ALIAS: Regex = Regex::new(r#"(?P<name>[\S]{1,12})(?:(?: (?P<cmd>.*)$)|$)"#).unwrap();
}

#[command]
#[supports_dm(false)]
#[permission_level(Restricted)]
#[can_blacklist(false)]
async fn blacklist(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let capture_opt = REGEX_CHANNEL.captures(&args).map(|cap| cap.get(1)).flatten();

    let mut channel = match capture_opt {
        Some(capture) =>
            ChannelData::from_id(capture.as_str().parse::<u64>().unwrap(), &pool).await.unwrap(),

        None =>
            ChannelData::from_channel(msg.channel(&ctx).await.unwrap(), &pool).await.unwrap(),
    };

    channel.blacklisted = !channel.blacklisted;
    channel.commit_changes(&pool).await;

    if channel.blacklisted {
        let _ = msg.channel_id.say(&ctx, "Blacklisted").await;
    }
    else {
        let _ = msg.channel_id.say(&ctx, "Unblacklisted").await;
    }

    Ok(())
}

#[command]
async fn timezone(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let mut user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();
    let guild_data = GuildData::from_guild(msg.guild(&ctx).await.unwrap(), &pool).await.unwrap();

    if !args.is_empty() {
        match args.parse::<Tz>() {
            Ok(_) => {
                user_data.timezone = args;
                user_data.commit_changes(&pool).await;

                let now = Utc::now().with_timezone(&user_data.timezone());

                let content = user_data.response(&pool, "timezone/set_p").await
                    .replacen("{timezone}", &user_data.timezone, 1)
                    .replacen("{time}", &now.format("%H:%M").to_string(), 1);

                let _ = msg.channel_id.say(&ctx, content).await;
            }

            Err(_) => {
                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "timezone/no_timezone").await).await;
            }
        }
    }
    else {
        let content = user_data.response(&pool, "timezone/no_argument").await
            .replace("{prefix}", &guild_data.prefix)
            .replacen("{timezone}", &user_data.timezone, 1);

        let _ = msg.channel_id.say(&ctx, content).await;
    }

    Ok(())
}

#[command]
async fn language(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let mut user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();

    match sqlx::query!(
        "
SELECT code FROM languages WHERE code = ? OR name = ?
        ", args, args)
        .fetch_one(&pool)
        .await {

        Ok(row) => {
            user_data.language = row.code;

            user_data.commit_changes(&pool).await;

            let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "lang/set_p").await).await;
        },

        Err(_) => {
            let language_codes = sqlx::query!("SELECT name, code FROM languages")
                .fetch_all(&pool)
                .await
                .unwrap()
                .iter()
                .map(|language| format!("{} ({})", language.name.to_title_case(), language.code.to_uppercase()))
                .collect::<Vec<String>>()
                .join("\n");

            let content = user_data.response(&pool, "lang/invalid").await
                .replacen("{}", &language_codes, 1);

            let _ = msg.channel_id.say(&ctx, content).await;
        },
    }

    Ok(())
}

#[command]
#[supports_dm(false)]
#[permission_level(Restricted)]
async fn prefix(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let mut guild_data = GuildData::from_guild(msg.guild(&ctx).await.unwrap(), &pool).await.unwrap();
    let user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();

    if args.len() > 5 {
        let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "prefix/too_long").await).await;
    }
    else if args.is_empty() {
        let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "prefix/no_argument").await).await;
    }
    else {
        guild_data.prefix = args;
        guild_data.commit_changes(&pool).await;

        let content = user_data.response(&pool, "prefix/success").await
            .replacen("{prefix}", &guild_data.prefix, 1);

        let _ = msg.channel_id.say(&ctx, content).await;
    }

    Ok(())
}

#[command]
#[supports_dm(false)]
#[permission_level(Restricted)]
async fn restrict(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();
    let guild_data = GuildData::from_guild(msg.guild(&ctx).await.unwrap(), &pool).await.unwrap();

    let role_tag_match = REGEX_ROLE.find(&args);

    if let Some(role_tag) = role_tag_match {
        let commands = REGEX_COMMANDS.find_iter(&args.to_lowercase()).map(|c| c.as_str().to_string()).collect::<Vec<String>>();
        let role_id = RoleId(role_tag.as_str()[3..role_tag.as_str().len()-1].parse::<u64>().unwrap());

        let role_opt = role_id.to_role_cached(&ctx).await;

        if let Some(role) = role_opt {
            if commands.is_empty() {
                let _ = sqlx::query!(
                    "
DELETE FROM command_restrictions WHERE role_id = (SELECT id FROM roles WHERE role = ?)
                    ", role.id.as_u64())
                    .execute(&pool)
                    .await;

                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "restrict/disabled").await).await;
            }
            else {
                let _ = sqlx::query!(
                    "
INSERT IGNORE INTO roles (role, name, guild_id) VALUES (?, ?, ?)
                    ", role.id.as_u64(), role.name, guild_data.id)
                    .execute(&pool)
                    .await;

                for command in commands {
                    let res = sqlx::query!(
                        "
INSERT INTO command_restrictions (role_id, command) VALUES ((SELECT id FROM roles WHERE role = ?), ?)
                        ", role.id.as_u64(), command)
                        .execute(&pool)
                        .await;

                    if res.is_err() {
                        let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "restrict/failure").await).await;
                    }
                }

                let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "restrict/enabled").await).await;
            }
        }
    }
    else if args.is_empty() {
        let guild_id = msg.guild_id.unwrap().as_u64().to_owned();

        let rows = sqlx::query!(
            "
SELECT
    roles.role, command_restrictions.command
FROM
    command_restrictions
INNER JOIN
    roles
ON
    roles.id = command_restrictions.role_id
WHERE
    roles.guild_id = (SELECT id FROM guilds WHERE guild = ?)
            ", guild_id)
            .fetch_all(&pool)
            .await
            .unwrap();

        let display_inner = rows.iter().map(|row| format!("<@&{}> can use {}", row.role, row.command)).collect::<Vec<String>>().join("\n");
        let display = user_data.response(&pool, "restrict/allowed").await.replacen("{}", &display_inner, 1);

        let _ = msg.channel_id.say(&ctx, display).await;
    }
    else {
        let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "restrict/help").await).await;
    }

    Ok(())
}

#[command]
#[supports_dm(false)]
#[permission_level(Managed)]
async fn alias(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_user(&msg.author, &ctx, &pool).await.unwrap();

    let guild_id = msg.guild_id.unwrap().as_u64().to_owned();

    let matches_opt = REGEX_ALIAS.captures(&args);

    if let Some(matches) = matches_opt {
        let name = matches.name("name").unwrap().as_str();
        let command_opt = matches.name("cmd").map(|m| m.as_str());

        match name {
            "list" => {
                let aliases = sqlx::query!(
                    "
SELECT name, command FROM command_aliases WHERE guild_id = (SELECT id FROM guilds WHERE guild = ?)
                    ", guild_id)
                    .fetch_all(&pool)
                    .await
                    .unwrap();

                let content = iter::once("Aliases:".to_string())
                    .chain(
                        aliases
                            .iter()
                            .map(|row| format!("**{}**: `{}`", row.name, row.command)
                        )
                    );

                let _ = msg.channel_id.say_lines(&ctx, content).await;
            },

            "remove" => {
                if let Some(command) = command_opt {
                    let deleted_count = sqlx::query!(
                        "
SELECT COUNT(1) AS count FROM command_aliases WHERE name = ? AND guild_id = (SELECT id FROM guilds WHERE guild = ?)
                        ", command, guild_id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();

                    sqlx::query!(
                        "
DELETE FROM command_aliases WHERE name = ? AND guild_id = (SELECT id FROM guilds WHERE guild = ?)
                        ", command, guild_id)
                        .execute(&pool)
                        .await
                        .unwrap();

                    let content = user_data.response(&pool, "alias/removed").await.replace("{count}", &deleted_count.count.to_string());

                    let _ = msg.channel_id.say(&ctx, content).await;
                }
                else {
                    let _ = msg.channel_id.say(&ctx, user_data.response(&pool, "alias/help").await).await;
                }
            },

            name => {
                if let Some(command) = command_opt {
                    let res = sqlx::query!(
                        "
INSERT INTO command_aliases (guild_id, name, command) VALUES ((SELECT id FROM guilds WHERE guild = ?), ?, ?)
                        ", guild_id, name, command)
                        .execute(&pool)
                        .await;

                    if res.is_err() {
                        sqlx::query!(
                            "
UPDATE command_aliases SET command = ? WHERE guild_id = (SELECT id FROM guilds WHERE guild = ?) AND name = ?
                            ", command, guild_id, name)
                            .execute(&pool)
                            .await
                            .unwrap();
                    }

                    let content = user_data.response(&pool, "alias/created").await.replace("{name}", name);

                    let _ = msg.channel_id.say(&ctx, content).await;
                }
                else {
                    match sqlx::query!(
                        "
SELECT command FROM command_aliases WHERE guild_id = (SELECT id FROM guilds WHERE guild = ?) AND name = ?
                        ", guild_id, name)
                        .fetch_one(&pool)
                        .await {

                        Ok(row) => {
                            let framework = ctx.data.read().await
                                .get::<FrameworkCtx>().cloned().expect("Could not get FrameworkCtx from data");

                            let mut new_msg = msg.clone();
                            new_msg.content = format!("<@{}> {}", &ctx.cache.current_user_id().await, row.command);

                            framework.dispatch(ctx.clone(), new_msg).await;
                        },

                        Err(_) => {
                            let content = user_data.response(&pool, "alias/not_found").await.replace("{name}", name);

                            let _ = msg.channel_id.say(&ctx, content).await;
                        },
                    }
                }
            }
        }
    }
    else {
        let prefix = GuildData::prefix_from_id(msg.guild_id, &pool).await;
        let content = user_data.response(&pool, "alias/help").await.replace("{prefix}", &prefix);

        let _ = msg.channel_id.say(&ctx, content).await;
    }

    Ok(())
}
