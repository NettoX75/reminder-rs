#![feature(int_roundings)]
#[macro_use]
extern crate lazy_static;

mod commands;
mod component_models;
mod consts;
mod framework;
mod hooks;
mod models;
mod time_parser;

use std::{collections::HashMap, env, sync::Arc, time::Instant};

use chrono_tz::Tz;
use dashmap::DashMap;
use dotenv::dotenv;
use log::info;
use serenity::{
    async_trait,
    cache::Cache,
    client::{bridge::gateway::GatewayIntents, Client},
    futures::TryFutureExt,
    http::{client::Http, CacheHttp},
    model::{
        channel::{GuildChannel, Message},
        gateway::{Activity, Ready},
        guild::{Guild, GuildUnavailable},
        id::{GuildId, UserId},
        interactions::Interaction,
    },
    prelude::{Context, EventHandler, TypeMapKey},
    utils::shard_id,
};
use sqlx::mysql::MySqlPool;
use tokio::sync::RwLock;

use crate::{
    commands::{info_cmds, moderation_cmds, reminder_cmds, todo_cmds},
    component_models::ComponentDataModel,
    consts::{CNC_GUILD, DEFAULT_PREFIX, SUBSCRIPTION_ROLES, THEME_COLOR},
    framework::RegexFramework,
    models::{command_macro::CommandMacro, guild_data::GuildData},
};

struct GuildDataCache;

impl TypeMapKey for GuildDataCache {
    type Value = Arc<DashMap<GuildId, Arc<RwLock<GuildData>>>>;
}

struct SQLPool;

impl TypeMapKey for SQLPool {
    type Value = MySqlPool;
}

struct ReqwestClient;

impl TypeMapKey for ReqwestClient {
    type Value = Arc<reqwest::Client>;
}

struct PopularTimezones;

impl TypeMapKey for PopularTimezones {
    type Value = Arc<Vec<Tz>>;
}

struct CurrentlyExecuting;

impl TypeMapKey for CurrentlyExecuting {
    type Value = Arc<RwLock<HashMap<UserId, Instant>>>;
}

struct RecordingMacros;

impl TypeMapKey for RecordingMacros {
    type Value = Arc<RwLock<HashMap<(GuildId, UserId), CommandMacro>>>;
}

#[async_trait]
trait LimitExecutors {
    async fn check_executing(&self, user: UserId) -> bool;
    async fn set_executing(&self, user: UserId);
    async fn drop_executing(&self, user: UserId);
}

#[async_trait]
impl LimitExecutors for Context {
    async fn check_executing(&self, user: UserId) -> bool {
        let currently_executing =
            self.data.read().await.get::<CurrentlyExecuting>().cloned().unwrap();

        let lock = currently_executing.read().await;

        lock.get(&user).map_or(false, |now| now.elapsed().as_secs() < 4)
    }

    async fn set_executing(&self, user: UserId) {
        let currently_executing =
            self.data.read().await.get::<CurrentlyExecuting>().cloned().unwrap();

        let mut lock = currently_executing.write().await;

        lock.insert(user, Instant::now());
    }

    async fn drop_executing(&self, user: UserId) {
        let currently_executing =
            self.data.read().await.get::<CurrentlyExecuting>().cloned().unwrap();

        let mut lock = currently_executing.write().await;

        lock.remove(&user);
    }
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn cache_ready(&self, ctx: Context, _: Vec<GuildId>) {
        let framework = ctx
            .data
            .read()
            .await
            .get::<RegexFramework>()
            .cloned()
            .expect("RegexFramework not found in context");

        framework.build_slash(ctx).await;
    }

    async fn channel_delete(&self, ctx: Context, channel: &GuildChannel) {
        let pool = ctx
            .data
            .read()
            .await
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        sqlx::query!(
            "
DELETE FROM channels WHERE channel = ?
            ",
            channel.id.as_u64()
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    async fn guild_create(&self, ctx: Context, guild: Guild, is_new: bool) {
        if is_new {
            let guild_id = guild.id.as_u64().to_owned();

            {
                let pool = ctx
                    .data
                    .read()
                    .await
                    .get::<SQLPool>()
                    .cloned()
                    .expect("Could not get SQLPool from data");

                GuildData::from_guild(guild, &pool).await.unwrap_or_else(|_| {
                    panic!("Failed to create new guild object for {}", guild_id)
                });
            }

            if let Ok(token) = env::var("DISCORDBOTS_TOKEN") {
                let shard_count = ctx.cache.shard_count();
                let current_shard_id = shard_id(guild_id, shard_count);

                let guild_count = ctx
                    .cache
                    .guilds()
                    .iter()
                    .filter(|g| shard_id(g.as_u64().to_owned(), shard_count) == current_shard_id)
                    .count() as u64;

                let mut hm = HashMap::new();
                hm.insert("server_count", guild_count);
                hm.insert("shard_id", current_shard_id);
                hm.insert("shard_count", shard_count);

                let client = ctx
                    .data
                    .read()
                    .await
                    .get::<ReqwestClient>()
                    .cloned()
                    .expect("Could not get ReqwestClient from data");

                let response = client
                    .post(
                        format!(
                            "https://top.gg/api/bots/{}/stats",
                            ctx.cache.current_user_id().as_u64()
                        )
                        .as_str(),
                    )
                    .header("Authorization", token)
                    .json(&hm)
                    .send()
                    .await;

                if let Err(res) = response {
                    println!("DiscordBots Response: {:?}", res);
                }
            }
        }
    }

    async fn guild_delete(
        &self,
        ctx: Context,
        deleted_guild: GuildUnavailable,
        _guild: Option<Guild>,
    ) {
        let pool = ctx
            .data
            .read()
            .await
            .get::<SQLPool>()
            .cloned()
            .expect("Could not get SQLPool from data");

        let guild_data_cache = ctx.data.read().await.get::<GuildDataCache>().cloned().unwrap();
        guild_data_cache.remove(&deleted_guild.id);

        sqlx::query!(
            "
DELETE FROM guilds WHERE guild = ?
            ",
            deleted_guild.id.as_u64()
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    async fn ready(&self, ctx: Context, _: Ready) {
        ctx.set_activity(Activity::watching("for /remind")).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::ApplicationCommand(application_command) => {
                if application_command.guild_id.is_none() {
                    return;
                }

                let framework = ctx
                    .data
                    .read()
                    .await
                    .get::<RegexFramework>()
                    .cloned()
                    .expect("RegexFramework not found in context");

                framework.execute(ctx, application_command).await;
            }
            Interaction::MessageComponent(component) => {
                let component_model = ComponentDataModel::from_custom_id(&component.data.custom_id);
                component_model.act(&ctx, component).await;
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init();

    dotenv()?;

    let token = env::var("DISCORD_TOKEN").expect("Missing DISCORD_TOKEN from environment");

    let http = Http::new_with_token(&token);

    let logged_in_id = http.get_current_user().map_ok(|user| user.id.as_u64().to_owned()).await?;
    let application_id = http.get_current_application_info().await?.id;

    let dm_enabled = env::var("DM_ENABLED").map_or(true, |var| var == "1");

    let framework = RegexFramework::new(logged_in_id)
        .default_prefix(DEFAULT_PREFIX.clone())
        .case_insensitive(env::var("CASE_INSENSITIVE").map_or(true, |var| var == "1"))
        .ignore_bots(env::var("IGNORE_BOTS").map_or(true, |var| var == "1"))
        .debug_guild(env::var("DEBUG_GUILD").map_or(None, |g| {
            Some(GuildId(g.parse::<u64>().expect("DEBUG_GUILD must be a guild ID")))
        }))
        .dm_enabled(dm_enabled)
        // info commands
        //.add_command("help", &info_cmds::HELP_COMMAND)
        .add_command(&info_cmds::INFO_COMMAND)
        .add_command(&info_cmds::DONATE_COMMAND)
        .add_command(&info_cmds::DASHBOARD_COMMAND)
        .add_command(&info_cmds::CLOCK_COMMAND)
        // reminder commands
        .add_command(&reminder_cmds::TIMER_COMMAND)
        .add_command(&reminder_cmds::REMIND_COMMAND)
        // management commands
        .add_command(&reminder_cmds::DELETE_COMMAND)
        .add_command(&reminder_cmds::LOOK_COMMAND)
        .add_command(&reminder_cmds::PAUSE_COMMAND)
        .add_command(&reminder_cmds::OFFSET_COMMAND)
        .add_command(&reminder_cmds::NUDGE_COMMAND)
        // to-do commands
        .add_command(&todo_cmds::TODO_COMMAND)
        // moderation commands
        .add_command(&moderation_cmds::RESTRICT_COMMAND)
        .add_command(&moderation_cmds::TIMEZONE_COMMAND)
        .add_command(&moderation_cmds::MACRO_CMD_COMMAND)
        .add_hook(&hooks::CHECK_SELF_PERMISSIONS_HOOK)
        .add_hook(&hooks::MACRO_CHECK_HOOK)
        .build();

    let framework_arc = Arc::new(framework);

    let mut client = Client::builder(&token)
        .intents(if dm_enabled {
            GatewayIntents::GUILD_MESSAGES
                | GatewayIntents::GUILDS
                | GatewayIntents::DIRECT_MESSAGES
        } else {
            GatewayIntents::GUILD_MESSAGES | GatewayIntents::GUILDS
        })
        .application_id(application_id.0)
        .event_handler(Handler)
        .framework_arc(framework_arc.clone())
        .await
        .expect("Error occurred creating client");

    {
        let guild_data_cache = dashmap::DashMap::new();

        let pool = MySqlPool::connect(
            &env::var("DATABASE_URL").expect("Missing DATABASE_URL from environment"),
        )
        .await
        .unwrap();

        let popular_timezones = sqlx::query!(
            "SELECT timezone FROM users GROUP BY timezone ORDER BY COUNT(timezone) DESC LIMIT 21"
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|t| t.timezone.parse::<Tz>().unwrap())
        .collect::<Vec<Tz>>();

        let mut data = client.data.write().await;

        data.insert::<GuildDataCache>(Arc::new(guild_data_cache));
        data.insert::<CurrentlyExecuting>(Arc::new(RwLock::new(HashMap::new())));
        data.insert::<SQLPool>(pool);
        data.insert::<PopularTimezones>(Arc::new(popular_timezones));
        data.insert::<ReqwestClient>(Arc::new(reqwest::Client::new()));
        data.insert::<RegexFramework>(framework_arc.clone());
        data.insert::<RecordingMacros>(Arc::new(RwLock::new(HashMap::new())));
    }

    if let Ok((Some(lower), Some(upper))) = env::var("SHARD_RANGE").map(|sr| {
        let mut split =
            sr.split(',').map(|val| val.parse::<u64>().expect("SHARD_RANGE not an integer"));

        (split.next(), split.next())
    }) {
        let total_shards = env::var("SHARD_COUNT")
            .map(|shard_count| shard_count.parse::<u64>().ok())
            .ok()
            .flatten()
            .expect("No SHARD_COUNT provided, but SHARD_RANGE was provided");

        assert!(lower < upper, "SHARD_RANGE lower limit is not less than the upper limit");

        info!("Starting client fragment with shards {}-{}/{}", lower, upper, total_shards);

        client.start_shard_range([lower, upper], total_shards).await?;
    } else if let Ok(total_shards) = env::var("SHARD_COUNT")
        .map(|shard_count| shard_count.parse::<u64>().expect("SHARD_COUNT not an integer"))
    {
        info!("Starting client with {} shards", total_shards);

        client.start_shards(total_shards).await?;
    } else {
        info!("Starting client as autosharded");

        client.start_autosharded().await?;
    }

    Ok(())
}

pub async fn check_subscription(cache_http: impl CacheHttp, user_id: impl Into<UserId>) -> bool {
    if let Some(subscription_guild) = *CNC_GUILD {
        let guild_member = GuildId(subscription_guild).member(cache_http, user_id).await;

        if let Ok(member) = guild_member {
            for role in member.roles {
                if SUBSCRIPTION_ROLES.contains(role.as_u64()) {
                    return true;
                }
            }
        }

        false
    } else {
        true
    }
}

pub async fn check_subscription_on_message(
    cache_http: impl CacheHttp + AsRef<Cache>,
    msg: &Message,
) -> bool {
    check_subscription(&cache_http, &msg.author).await
        || if let Some(guild) = msg.guild(&cache_http) {
            check_subscription(&cache_http, guild.owner_id).await
        } else {
            false
        }
}
