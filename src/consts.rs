pub const PREFIX: &str = "$";

pub const DAY: u64 = 86_400;
pub const HOUR: u64 = 3_600;
pub const MINUTE: u64 = 60;

pub const CHARACTERS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";

use std::{
    iter::FromIterator,
    env,
    collections::HashSet,
};

use lazy_static;

use regex::Regex;

lazy_static! {
    pub static ref SUBSCRIPTION_ROLES: HashSet<u64> = HashSet::from_iter(env::var("SUBSCRIPTION_ROLES")
        .map(
            |var| var
                .split(',')
                .filter_map(|item| {
                    item.parse::<u64>().ok()
                })
                .collect::<Vec<u64>>()
        ).unwrap_or_else(|_| vec![]));

    pub static ref CNC_GUILD: Option<u64> = env::var("CNC_GUILD").map(|var| var.parse::<u64>().ok()).ok().flatten();

    pub static ref REGEX_CHANNEL: Regex = Regex::new(r#"^\s*<#(\d+)>\s*$"#).unwrap();

    pub static ref REGEX_ROLE: Regex = Regex::new(r#"<@&([0-9]+)>"#).unwrap();

    pub static ref REGEX_COMMANDS: Regex = Regex::new(r#"([a-z]+)"#).unwrap();

    pub static ref REGEX_ALIAS: Regex = Regex::new(r#"(?P<name>[\S]{1,12})(?:(?: (?P<cmd>.*)$)|$)"#).unwrap();

    pub static ref REGEX_CHANNEL_USER: Regex = Regex::new(r#"^\s*<(#|@)(?:!)?(\d+)>\s*$"#).unwrap();

    pub static ref MIN_INTERVAL: i64 = env::var("MIN_INTERVAL").ok().map(|inner| inner.parse::<i64>().ok()).flatten().unwrap_or(600);

    pub static ref MAX_TIME: i64 = env::var("MAX_TIME").ok().map(|inner| inner.parse::<i64>().ok()).flatten().unwrap_or(60*60*24*365*50);

    pub static ref LOCAL_TIMEZONE: String = env::var("LOCAL_TIMEZONE").unwrap_or_else(|_| "UTC".to_string());

    pub static ref PYTHON_LOCATION: String = env::var("PYTHON_LOCATION").unwrap_or_else(|_| "venv/bin/python3".to_string());
}
