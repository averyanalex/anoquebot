use std::{collections::BTreeMap, future::Future, str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use db::Db;
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

mod db;

type Bot = Throttle<teloxide::Bot>;

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Start,
    WaitMessage {
        recipient: i64,
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
        .branch(dptree::case![State::WaitMessage { recipient }].endpoint(send_anon_message))
        .branch(dptree::endpoint(invalid_command));

    let db = Arc::new(Db::new().await?);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![db, InMemStorage::<State>::new()])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn get_personal_link(db: &Db, id: i64, invited_by: Option<i64>) -> Result<String> {
    let link = db.get_user_link(id, invited_by).await?;
    Ok(format!("https://t.me/anoquebot?start={link}"))
}

async fn get_your_link(db: &Db, id: i64, invited_by: Option<i64>) -> Result<String> {
    let personal_link = get_personal_link(db, id, invited_by).await?;
    Ok(format!(
        "Ваша персональная ссылка: {personal_link}. Отправьте \
                    её друзьям, чтобы начать получать анонимные вопросы!",
    ))
}

async fn try_handle(
    chat: &Chat,
    bot: &Bot,
    handle: impl Future<Output = Result<()>>,
) -> Result<()> {
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
        bot.send_message(chat.id, format!("Произошла неизвестная ошибка: {e}"))
            .await
            .ok();
    }

    sentry::end_session();

    Ok(())
}

async fn answer_cmd(
    db: Arc<Db>,
    bot: Bot,
    msg: Message,
    cmd: Command,
    dialogue: MyDialogue,
) -> Result<()> {
    try_handle(&msg.chat, &bot, async {
        let args = parse_command(msg.text().context("no text")?, "anoquebot")
                    .context("can't parse")?;
        let (link_present, recipient) = if let Some(link) = args.1.first() {
            (true, db.user_id_by_link(link).await?)} else {(false, None)};
        let get_your_link = get_your_link(&db, msg.chat.id.0, recipient).await?;

        match cmd {
            Command::Start => {
                if link_present {
                    if let Some(recipient) = recipient {
                        bot.send_message(msg.chat.id, "Напишите анонимный вопрос:")
                            .await?;
                        dialogue.update(State::WaitMessage { recipient }).await?;
                    } else {
                        bot.send_message(
                            msg.chat.id,
                            format!("Ссылка недействительна! Попросите автора создать новую ссылку.\n\n{get_your_link}"),
                        )
                        .await?;
                    };
                } else {
                    bot.send_message(
                        msg.chat.id,
                        get_your_link
                    )
                    .await?;
                    dialogue.update(State::Start).await?;
                }
            }
        };
        Ok(())
    })
    .await
}

async fn send_anon_message(
    db: Arc<Db>,
    bot: Bot,
    msg: Message,
    dialogue: MyDialogue,
    recipient: i64,
) -> Result<()> {
    try_handle(&msg.chat, &bot, async {
        let get_your_link = get_your_link(&db, msg.chat.id.0, Some(recipient)).await?;

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
                ChatId(recipient),
                format!("Вы получили анонимное сообщение:\n\n{text}"),
            )
            .await
        {
            bot.send_message(
                msg.chat.id,
                format!(
                    "Не удалось отправить сообщение: {e}. \
            Возможно, получатель заблокировал бота.\n\n{get_your_link}"
                ),
            )
            .await?;
            sentry::capture_error(&e);
        } else {
            bot.send_message(
                msg.chat.id,
                format!("Ваше сообщение успешно отправлено!\n\n{get_your_link}",),
            )
            .await?;
        }

        dialogue.update(State::Start).await?;

        Ok(())
    })
    .await
}

async fn invalid_command(db: Arc<Db>, bot: Bot, msg: Message) -> Result<()> {
    try_handle(&msg.chat, &bot, async {
        let get_your_link = get_your_link(&db, msg.chat.id.0, None).await?;

        bot.send_message(
            msg.chat.id,
            format!("Чтобы отправить кому-то анонимный вопрос, перейдите по его персональной ссылке.\n\n{get_your_link}"),
        )
        .await?;
        Ok(())
    })
    .await
}
