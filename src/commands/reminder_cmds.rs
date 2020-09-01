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

use chrono_tz::{
    Tz,
    Etc::UTC,
};

use crate::{
    models::{
        ChannelData,
        UserData,
    },
    SQLPool,
    framework::SendFromDb,
    time_parser::TimeParser,
};

use chrono::NaiveDateTime;


#[command]
async fn pause(ctx: &Context, msg: &Message, args: String) -> CommandResult {
    let pool = ctx.data.read().await
        .get::<SQLPool>().cloned().expect("Could not get SQLPool from data");

    let user_data = UserData::from_id(&msg.author, &ctx, pool.clone()).await.unwrap();
    let mut channel = ChannelData::from_channel(msg.channel(&ctx).await.unwrap(), pool.clone()).await.unwrap();

    if args.len() == 0 {
        channel.paused = !channel.paused;
        channel.paused_until = None;

        channel.commit_changes(pool).await;

        if channel.paused {
            let _ = msg.channel_id.say_named(&ctx, user_data.language, "paused/paused_indefinite").await;
        }
        else {
            let _ = msg.channel_id.say_named(&ctx, user_data.language, "paused/unpaused").await;
        }
    }
    else {
        let parser = TimeParser::new(args, user_data.timezone.parse().unwrap());
        let pause_until = parser.timestamp();

        match pause_until {
            Ok(timestamp) => {
                channel.paused = true;
                channel.paused_until = Some(NaiveDateTime::from_timestamp(timestamp, 0));

                channel.commit_changes(pool).await;

                let _ = msg.channel_id.say_named(&ctx, user_data.language, "paused/paused_until").await;
            },

            Err(_) => {
                let _ = msg.channel_id.say_named(&ctx, user_data.language, "paused/invalid_time").await;
            },
        }
    }

    Ok(())
}
