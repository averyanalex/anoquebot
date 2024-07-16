#![feature(let_chains)]

use std::{collections::BTreeMap, future::Future, str::FromStr, sync::Arc};

use anyhow::{ensure, Result};
use db::Db;
use sentry::protocol::Value;
use teloxide::{
    adaptors::{throttle::Limits, Throttle},
    dispatching::dialogue::InMemStorage,
    payloads::AnswerCallbackQuerySetters,
    prelude::*,
    types::{
        Chat, InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup,
        KeyboardRemove, MessageId,
    },
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
    WaitNewMessage {
        recipient_id: i64,
    },
}

type MyDialogue = Dialogue<State, InMemStorage<State>>;

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
            sentry::integrations::tracing::layer().event_filter(|md| match *md.level() {
                Level::TRACE => sentry::integrations::tracing::EventFilter::Ignore,
                _ => sentry::integrations::tracing::EventFilter::Breadcrumb,
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
                    default_integrations: true,
                    attach_stacktrace: true,
                    send_default_pii: true,
                    max_breadcrumbs: 300,
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

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .enter_dialogue::<Message, InMemStorage<State>, State>()
                .branch(dptree::endpoint(handle_message)),
        )
        .branch(Update::filter_callback_query().endpoint(handle_callback_query));

    let db = Arc::new(Db::new().await?);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![db, InMemStorage::<State>::new()])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn user_link(bot: &Bot, link_code: &str) -> Result<String> {
    let mut tme_url = bot.get_me().await?.tme_url();
    tme_url.set_query(Some(&format!("start={link_code}")));
    Ok(tme_url.to_string())
}

async fn reset_dialogue(bot: &Bot, dialogue: &MyDialogue, chat_id: ChatId) -> Result<()> {
    let state = dialogue.get_or_default().await?;
    match state {
        State::Start => {}
        State::WaitNewMessage { recipient_id: _ } => {
            bot.send_message(chat_id, "Отправка сообщения отменена!")
                .reply_markup(KeyboardRemove::new())
                .await?;
        }
    };
    dialogue.reset().await?;
    Ok(())
}

async fn forward_message(
    bot: &Bot,
    msg: &Message,
    recipient: ChatId,
    reply_for: Option<MessageId>,
) -> Result<Option<MessageId>> {
    let inline_keyboard =
        InlineKeyboardMarkup::new([[InlineKeyboardButton::callback("Ответить", "reply")]]);

    let mut req = bot
        .copy_message(recipient, msg.chat.id, msg.id)
        .disable_notification(false)
        .allow_sending_without_reply(true)
        .reply_markup(inline_keyboard);
    if let Some(reply_for) = reply_for {
        req = req.reply_to_message_id(reply_for);
    }

    let sent_msg = req.await?;

    Ok(Some(sent_msg))
}

async fn handle_message(db: Arc<Db>, bot: Bot, msg: Message, dialogue: MyDialogue) -> Result<()> {
    try_handle(&msg.chat, &bot, async {
        if let Some(text) = msg.text() && let Some(cmd) = parse_command(text, bot.get_me().await?.username()) {
            // received command
            reset_dialogue(&bot, &dialogue, msg.chat.id).await?;
            match cmd.0 {
                "start" => {
                    // received start command
                    if let Some(link) = cmd.1.into_iter().next() {
                        // start command has link
                        if let Some(recipient_id) = db.user_id_by_link(link).await? {
                            // link is valid
                            db.get_user_link(msg.chat.id.0, Some(recipient_id)).await?;
                            bot.send_message(msg.chat.id, "Отправьте ваше анонимное сообщение \
                            (поддерживаются любые типы сообщений):")
                                .reply_markup(KeyboardMarkup::new([[KeyboardButton::new("Отмена")]]).resize_keyboard(true))
                                .await?;
                            dialogue.update(State::WaitNewMessage { recipient_id }).await?;
                        } else {
                            // link is invalid
                            let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
                            bot.send_message(
                                msg.chat.id,
                                format!("Ссылка недействительна! Попросите автора создать новую ссылку. \
                                А вот, кстати, ваша собственная ссылка для получения анонимных вопросов и сообщений: {}", user_link(&bot, &my_link_code).await?),
                            )
                            .reply_markup(KeyboardRemove::new())
                            .await?;
                        }
                    } else {
                        // no link
                        let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
                        bot.send_message(msg.chat.id, format!("Добро пожаловать в бот для полученя анонимных вопросов и сообщений! \
                        Чтобы начать получать анонимные вопросы, поделитесь своей персональной ссылкой с друзьями: {}. Возможна отправка любых типов сообщений.", user_link(&bot, &my_link_code).await?)).await?;
                    }
                }
                _ => {
                    db.get_user_link(msg.chat.id.0, None).await?;
                    bot.send_message(msg.chat.id, "Неизвестная команда! Попробуйте /start").await?;
                }
            }
        } else {
            let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
            let user_link = user_link(&bot, &my_link_code).await?;

            let state = dialogue.get_or_default().await?;

            match state {
                State::Start => {
                    // not waiting message
                    if let Some(msg_reply_to) = msg.reply_to_message() {
                        // user tries to reply
                        ensure!(msg_reply_to.chat.id == msg.chat.id);
                        if let Some(reply_for) = db.find_another_message(msg.chat.id.0, msg_reply_to.id.0).await? {
                            if let Some(sent_msg_id) = forward_message(&bot, &msg, ChatId(reply_for.0), Some(MessageId(reply_for.1))).await? {
                                db.save_message(msg.chat.id.0, msg.id.0, reply_for.0, sent_msg_id.0).await?;
                                bot.send_message(
                                    msg.chat.id,
                                    "Ваше сообщение отправлено!"
                                )
                                .reply_markup(KeyboardRemove::new())
                                .await?;
                            }
                        } else {
                            bot.send_message(msg.chat.id, "Отвечать (свайпать слево) можно только на входящие и исходящие анонимные сообщения")
                                .reply_markup(KeyboardRemove::new())
                                .await?;
                        }
                    } else {
                        bot.send_message(msg.chat.id, format!("Кажется, вы отправили сообщение, но мы его не ждали... Может быть, \
                        вы хотели отправить кому-то сообщение или ответить на полученное? В таком случае перейдите по ссылке друга или свайпните \
                        полученное сообщение влево. А если вы хотите начать получать сообщения сами, то держите ссылку: {user_link}"))
                            .reply_markup(KeyboardRemove::new())
                            .await?;
                    }
                },
                State::WaitNewMessage { recipient_id } => {
                    // waiting message
                    if let Some(text) = msg.text() && text == "Отмена" {
                        reset_dialogue(&bot, &dialogue, msg.chat.id).await?;
                    } else if msg.reply_to_message().is_some() {
                        bot.send_message(msg.chat.id, "Для отправки анонимного вопроса не нужно свайпать сообщение влево (отвечать). Попробуйте ещё раз, \
                        отменив отправку вопроса или не отвечая на другие сообщения.").await?;
                    } else if let Some(sent_msg_id) = forward_message(&bot, &msg, ChatId(recipient_id), None).await? {
                        db.save_message(msg.chat.id.0, msg.id.0, recipient_id, sent_msg_id.0).await?;
                        bot.send_message(
                            msg.chat.id,
                            format!("Ваше сообщение отправлено! А вот, кстати, ваша \
                                собственная ссылка для получения анонимных вопросов и сообщений: {user_link}")
                        )
                        .reply_markup(KeyboardRemove::new())
                        .await?;
                        dialogue.reset().await?;
                    }
                }
            }
        };
        Ok(())
    })
    .await
}

async fn handle_callback_query(bot: Bot, q: CallbackQuery) -> Result<()> {
    bot.answer_callback_query(q.id)
        .cache_time(3600)
        .text("Ответьте (свайпните влево) это сообщение для ответа")
        .await?;
    Ok(())
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
        sentry::integrations::anyhow::capture_anyhow(&e);
        bot.send_message(chat.id, format!("Произошла неизвестная ошибка: {e}"))
            .await
            .ok();
    }

    sentry::end_session();

    Ok(())
}
