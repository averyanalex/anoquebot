#![feature(let_chains)]

use std::{collections::BTreeMap, future::Future, str::FromStr, sync::Arc};

use anyhow::{ensure, Result};
use db::Db;
use sentry::protocol::Value;
use teloxide::{
    adaptors::{throttle::Limits, Throttle},
    dispatching::dialogue::InMemStorage,
    prelude::*,
    types::{Chat, InputFile, MessageId},
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
        sent_msg: i32,
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
        .branch(dptree::endpoint(handle_message));

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
        State::WaitNewMessage {
            sent_msg,
            recipient_id: _,
        } => {
            bot.delete_message(chat_id, MessageId(sent_msg)).await.ok();
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
) -> Result<Option<Message>> {
    macro_rules! prepare_msg {
        ($req:ident) => {
            if let Some(reply_for) = reply_for {
                $req = $req.reply_to_message_id(reply_for);
            }

            if let Some(caption) = msg.caption() {
                $req = $req.caption(format!("Вы получили сообщение (свайпните влево для ответа):\n\n{caption}"));
            } else {
                $req = $req.caption("Вы получили сообщение (свайпните влево для ответа)");
            }
        };
    }

    let sent_msg = if let Some([.., photo]) = msg.photo() {
        let mut r = bot.send_photo(recipient, InputFile::file_id(&photo.file.id));
        prepare_msg!(r);
        r.await?
    } else if let Some(video) = msg.video() {
        let mut r = bot.send_video(recipient, InputFile::file_id(&video.file.id));
        prepare_msg!(r);
        r.await?
    } else if let Some(audio) = msg.audio() {
        let mut r = bot.send_audio(recipient, InputFile::file_id(&audio.file.id));
        prepare_msg!(r);
        r.await?
    } else if let Some(document) = msg.document() {
        let mut r = bot.send_document(recipient, InputFile::file_id(&document.file.id));
        prepare_msg!(r);
        r.await?
    } else if let Some(voice) = msg.voice() {
        let mut r = bot.send_voice(recipient, InputFile::file_id(&voice.file.id));
        prepare_msg!(r);
        r.await?
    } else if let Some(video_note) = msg.video_note() {
        let mut r = bot.send_voice(recipient, InputFile::file_id(&video_note.file.id));
        prepare_msg!(r);
        r.await?
    } else if let Some(text) = msg.text() {
        let mut r = bot.send_message(
            recipient,
            format!("Вы получили сообщение (свайпните влево для ответа):\n\n{text}"),
        );
        if let Some(reply_for) = reply_for {
            r = r.reply_to_message_id(reply_for);
        }
        r.await?
    } else {
        bot.send_message(
            msg.chat.id,
            "Поддерживаются только текст, фото, видео, аудио, файлы, кружочки и голосовые.",
        )
        .await?;
        return Ok(None);
    };

    Ok(Some(sent_msg))
}

async fn handle_message(db: Arc<Db>, bot: Bot, msg: Message, dialogue: MyDialogue) -> Result<()> {
    try_handle(&msg.chat, &bot, async {
        if let Some(text) = msg.text() && let Some(cmd) = parse_command(text, bot.get_me().await?.username()) {
            match cmd.0 {
                "start" => {
                    if let Some(link) = cmd.1.into_iter().next() {
                        if let Some(recipient_id) = db.user_id_by_link(link).await? {
                            db.get_user_link(msg.chat.id.0, Some(recipient_id)).await?;
                            let sent_msg = bot.send_message(msg.chat.id, "Отправьте ваше анонимное сообщение \
                            (текст, фото, видео, аудио, файл, кружочек или голосовое):")
                                .await?;
                            reset_dialogue(&bot, &dialogue, msg.chat.id).await?;
                            dialogue.update(State::WaitNewMessage { recipient_id, sent_msg: sent_msg.id.0 }).await?;
                        } else {
                            let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
                            bot.send_message(
                                msg.chat.id,
                                format!("Ссылка недействительна! Попросите автора создать новую ссылку. \
                                А вот, кстати, ваша собственная ссылка для получения анонимных вопросов и сообщений: {}", user_link(&bot, &my_link_code).await?),
                            )
                            .await?;
                            reset_dialogue(&bot, &dialogue, msg.chat.id).await?;
                        }
                    } else {
                        let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
                        bot.send_message(msg.chat.id, format!("Добро пожаловать в бот для полученя анонимных вопросов и сообщений! \
                        Чтобы начать получать анонимные вопросы, поделитесь своей персональной ссылкой с друзьями: {}. Возможна отправка текста, фото, аудио, \
                        видео, файлов, кружочков и голосовых.", user_link(&bot, &my_link_code).await?)).await?;
                        reset_dialogue(&bot, &dialogue, msg.chat.id).await?;
                    }
                }
                _ => {
                    db.get_user_link(msg.chat.id.0, None).await?;
                    bot.send_message(msg.chat.id, "Неизвестная команда! Попробуйте /start").await?;
                    reset_dialogue(&bot, &dialogue, msg.chat.id).await?;
                }
            }
        } else {
            let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
            let user_link = user_link(&bot, &my_link_code).await?;

            let state = dialogue.get_or_default().await?;

            match state {
                State::Start => {
                    if let Some(msg_reply_to) = msg.reply_to_message() {
                        ensure!(msg_reply_to.chat.id == msg.chat.id);
                        if let Some(reply_for) = db.find_message(msg.chat.id.0, msg_reply_to.id.0).await? {
                            if let Some(sent_msg) = forward_message(&bot, &msg, ChatId(reply_for.0), Some(MessageId(reply_for.1))).await? {
                                bot.send_message(
                                    msg.chat.id,
                                    "Ваш ответ отправлен!"
                                )
                                .await?;
                                db.save_message(msg.chat.id.0, msg.id.0, sent_msg.chat.id.0, sent_msg.id.0).await?;
                            }
                        } else {
                            bot.send_message(msg.chat.id, "Отвечать (свайпать слево) можно только на входящие анонимные сообщения").await?;
                        }
                    } else {
                    bot.send_message(msg.chat.id, format!("Кажется, вы отправили сообщение, но мы его не ждали... Может быть, \
                    вы хотели отправить кому-то сообщение или ответь на полученное? В таком случае перейдите по ссылке друга или свайпните \
                    полученное сообщение слево. А если вы хотите начать получать сообщения сами, то держите ссылку: {user_link}")).await?;
                    }
                },
                State::WaitNewMessage { recipient_id, sent_msg: _ } => {
                    if msg.reply_to_message().is_some() {
                        bot.send_message(msg.chat.id, "Для отправки анонимного вопроса не нужно свайпать сообщение влево (отвечать)").await?;
                        dialogue.reset().await?;
                    } else {
                        if let Some(sent_msg) = forward_message(&bot, &msg, ChatId(recipient_id), None).await? {
                            bot.send_message(
                                msg.chat.id,
                                format!("Ваше сообщение отправлено! А вот, кстати, ваша \
                                    собственная ссылка для получения анонимных вопросов и сообщений: {user_link}")
                            )
                            .await?;
                            db.save_message(msg.chat.id.0, msg.id.0, sent_msg.chat.id.0, sent_msg.id.0).await?;
                        }
                        dialogue.reset().await?;
                    }
                }
            }
        };
        Ok(())
    })
    .await
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
