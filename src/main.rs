use std::{collections::BTreeMap, future::Future, str::FromStr};

use anyhow::{Context, Result};
use sentry::protocol::Value;
use teloxide::{
    adaptors::{throttle::Limits, Throttle},
    dispatching::dialogue::InMemStorage,
    macros::BotCommands,
    prelude::*,
    types::Chat,
    utils::command::parse_command,
};
use tracing::*;
use tracing_subscriber::prelude::*;

type Bot = Throttle<teloxide::Bot>;

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Start,
    WaitMessage {
        chat: i64,
    },
}

type MyDialogue = Dialogue<State, InMemStorage<State>>;

#[derive(Debug, BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Доступные команды:")]
enum Command {
    Start,
}

fn main() -> Result<()> {
    std::env::set_var("RUST_BACKTRACE", "1");

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer().with_filter(
                tracing_subscriber::filter::LevelFilter::from_str(
                    &std::env::var("RUST_LOG").unwrap_or_else(|_| String::from("info")),
                )
                .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO),
            ),
        )
        .with(
            sentry_tracing::layer().event_filter(|md| match *md.level() {
                Level::TRACE => sentry_tracing::EventFilter::Ignore,
                _ => sentry_tracing::EventFilter::Breadcrumb,
            }),
        )
        .try_init()
        .unwrap();

    let _sentry_guard = match std::env::var("SENTRY_DSN") {
        Ok(d) => {
            let guard = sentry::init((
                d,
                sentry::ClientOptions {
                    release: sentry::release_name!(),
                    attach_stacktrace: true,
                    traces_sample_rate: 1.0,
                    ..Default::default()
                },
            ));
            Some(guard)
        }
        Err(e) => {
            warn!("can't get SENTRY_DSN: {:?}", e);
            None
        }
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(_main())
}

async fn _main() -> Result<()> {
    tracing::info!("Starting bot...");
    let bot = teloxide::Bot::from_env().throttle(Limits::default());

    let handler = Update::filter_message()
        .enter_dialogue::<Message, InMemStorage<State>, State>()
        .branch(
            dptree::entry()
                .filter_command::<Command>()
                .endpoint(answer_cmd),
        )
        .branch(dptree::case![State::WaitMessage { chat }].endpoint(send_anon_message))
        .branch(dptree::endpoint(invalid_command));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![InMemStorage::<State>::new()])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn try_handle(chat: &Chat, handle: impl Future<Output = Result<()>>) -> Result<()> {
    sentry::start_session();
    sentry::configure_scope(|scope| {
        let mut map = BTreeMap::new();
        if let Some(first_name) = chat.first_name() {
            map.insert("first_name".to_owned(), Value::from(first_name));
        }
        if let Some(last_name) = chat.last_name() {
            map.insert("last_name".to_owned(), Value::from(last_name));
        }
        scope.set_user(Some(sentry::User {
            id: Some(chat.id.0.to_string()),
            username: chat.username().map(|u| u.to_owned()),
            other: map,
            ..Default::default()
        }));
    });

    if let Err(e) = handle.await {
        sentry_anyhow::capture_anyhow(&e);
    }

    sentry::end_session();

    Ok(())
}

async fn answer_cmd(bot: Bot, msg: Message, cmd: Command, dialogue: MyDialogue) -> Result<()> {
    try_handle(&msg.chat.clone(), async move {
        match cmd {
            Command::Start => {
                let args = parse_command(msg.text().context("no text")?, "anoquebot")
                    .context("cant parse")?;
                if let Some(chat) = args.1.first() {
                    let chat_id: i64 = chat.parse()?;
                    bot.send_message(msg.chat.id, "Напишите анонимный вопрос:")
                        .await?;
                    dialogue
                        .update(State::WaitMessage { chat: chat_id })
                        .await?;
                } else {
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "Ваша персональная ссылка: https://t.me/anoquebot?start={}. Отправьте \
                            её друзьям, чтобы начать получать анонимные вопросы!",
                            msg.chat.id.0
                        ),
                    )
                    .await?;
                    dialogue.update(State::Start).await?;
                }
            }
        }
        Ok(())
    })
    .await
}

async fn send_anon_message(bot: Bot, msg: Message, dialogue: MyDialogue, chat: i64) -> Result<()> {
    try_handle(&msg.chat.clone(), async move {
        let Some(text) = msg.text() else {
            bot.send_message(
                msg.chat.id,
                "В вашем сообщении нет текста! \
            Изображения на данный момент не поддерживаются. Попробуйте ещё раз.",
            )
            .await?;
            return Ok(());
        };

        if let Err(e) = bot
            .send_message(
                ChatId(chat),
                format!("Вы получили анонимное сообщение:\n\n{text}"),
            )
            .await
        {
            bot.send_message(
                msg.chat.id,
                format!(
                    "Не удалось отправить сообщение: {e}. \
            Возможно, получатель заблокировал бота."
                ),
            )
            .await?;
            sentry::capture_error(&e);
        } else {
            bot.send_message(
                msg.chat.id,
                format!(
                    "Ваше сообщение успешно отправлено!\n\n\
            Ваша персональная ссылка: https://t.me/anoquebot?start={}. Отправьте \
            её друзьям, чтобы начать получать анонимные вопросы!",
                    msg.chat.id
                ),
            )
            .await?;
        }

        dialogue.update(State::Start).await?;

        Ok(())
    })
    .await
}

async fn invalid_command(bot: Bot, msg: Message) -> Result<()> {
    try_handle(&msg.chat, async move {
        bot.send_message(
            msg.chat.id,
            "Используйте /start, чтобы получить персональную \
        ссылку, или перейдите по чей-то ссылке, чтобы отправить автору вопрос.",
        )
        .await?;
        Ok(())
    })
    .await
}
