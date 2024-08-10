#![feature(let_chains)]

use std::{str::FromStr, sync::Arc};

use anyhow::{ensure, Context, Result};
use db::Db;
use dptree::case;
use teloxide::{
    adaptors::{throttle::Limits, Throttle},
    dispatching::dialogue::{GetChatId, InMemStorage},
    macros::BotCommands,
    payloads::AnswerCallbackQuerySetters,
    prelude::*,
    types::{
        InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup, KeyboardRemove,
        Me, MessageId,
    },
    utils::command::BotCommands as _,
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

#[derive(Clone)]
struct UserLink(pub String);

impl UserLink {
    pub fn tme_url(&self, me: &Me) -> String {
        let mut tme_url = me.tme_url();
        tme_url.set_query(Some(&format!("start={}", self.0)));
        tme_url.to_string()
    }
}

#[derive(BotCommands, PartialEq, Debug, Clone)]
#[command(rename_rule = "lowercase")]
enum Command {
    #[command(description = "Получить свою ссылку")]
    Start(String),
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
    let bot = teloxide::Bot::from_env().throttle(Limits::default());

    let command_handler = teloxide::filter_command::<Command, _>()
        .branch(case![Command::Start(link)].endpoint(handle_command_start));

    let message_handler = Update::filter_message()
        .branch(command_handler)
        .map_async(|db: Arc<Db>, msg: Message| async move {
            db.get_user_link(msg.chat.id.0, None)
                .await
                .unwrap_or(UserLink("ERROR".to_owned()))
        })
        .branch(case![State::WaitNewMessage { recipient_id }].endpoint(handle_state_wait))
        .branch(dptree::endpoint(handle_state_start));

    let callback_handler = Update::filter_callback_query().endpoint(handle_callback_query);

    let handler = dptree::entry()
        .enter_dialogue::<Update, InMemStorage<State>, State>()
        .branch(message_handler)
        .branch(callback_handler);

    let db = Arc::new(Db::new().await?);

    bot.set_my_commands(Command::bot_commands()).await?;

    let me = bot.get_me().await?;
    let username = me.username();
    info!("starting bot @{username}");

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![db, InMemStorage::<State>::new()])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
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
    db: &Db,
    msg: &Message,
    recipient: ChatId,
    reply_for: Option<MessageId>,
) -> Result<MessageId> {
    let mut req = bot
        .copy_message(recipient, msg.chat.id, msg.id)
        .disable_notification(false)
        .allow_sending_without_reply(true);

    if db.answer_tip_enabled(recipient.0).await? {
        let inline_keyboard =
            InlineKeyboardMarkup::new([[InlineKeyboardButton::callback("Ответить", "reply")]]);
        req = req.reply_markup(inline_keyboard);
    }

    if let Some(reply_for) = reply_for {
        req = req.reply_to_message_id(reply_for);
    }

    Ok(req.await?)
}

async fn handle_command_start(
    bot: Bot,
    me: Me,
    msg: Message,
    link: String,
    db: Arc<Db>,
    dialogue: MyDialogue,
) -> Result<()> {
    reset_dialogue(&bot, &dialogue, msg.chat.id).await?;

    if link.is_empty() {
        let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
        bot.send_message(msg.chat.id, format!("Добро пожаловать! \
        Чтобы начать получать анонимные вопросы, поделитесь своей персональной ссылкой с друзьями: {}. \
        Возможна отправка любых типов сообщений: текстовых, фото, стикеров и прочих.", my_link_code.tme_url(&me))).await?;
    } else if let Some(recipient_id) = db.user_id_by_link(&link).await? {
        db.get_user_link(msg.chat.id.0, Some(recipient_id)).await?;
        bot.send_message(
            msg.chat.id,
            "Отправьте ваше анонимное сообщение (поддерживаются любые типы сообщений):",
        )
        .reply_markup(KeyboardMarkup::new([[KeyboardButton::new("Отмена")]]).resize_keyboard(true))
        .await?;
        dialogue
            .update(State::WaitNewMessage { recipient_id })
            .await?;
    } else {
        let my_link_code = db.get_user_link(msg.chat.id.0, None).await?;
        bot.send_message(
            msg.chat.id,
            format!("Ссылка недействительна! Попросите автора создать новую ссылку. \
            А вот, кстати, ваша собственная ссылка для получения анонимных вопросов и сообщений: {}", my_link_code.tme_url(&me)),
        )
        .reply_markup(KeyboardRemove::new())
        .await?;
    }
    Ok(())
}

async fn handle_state_start(
    db: Arc<Db>,
    bot: Bot,
    msg: Message,
    user_link: UserLink,
    me: Me,
) -> Result<()> {
    if let Some(msg_reply_to) = msg.reply_to_message() {
        ensure!(msg_reply_to.chat.id == msg.chat.id);
        if let Some(reply_for) = db
            .find_another_message(msg.chat.id.0, msg_reply_to.id.0)
            .await?
        {
            match forward_message(
                &bot,
                &db,
                &msg,
                ChatId(reply_for.0),
                Some(MessageId(reply_for.1)),
            )
            .await
            {
                Ok(sent_msg_id) => {
                    db.save_message(msg.chat.id.0, msg.id.0, reply_for.0, sent_msg_id.0)
                        .await?;
                    bot.send_message(msg.chat.id, "Ваше сообщение отправлено!")
                        .reply_markup(KeyboardRemove::new())
                        .await?;
                }
                Err(e) => {
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "Не удалось ответить на сообщение: {e}. Возможно, получатель заблокировал бота.",
                        ),
                    )
                    .reply_markup(KeyboardRemove::new())
                    .await?;
                }
            };
        } else {
            bot.send_message(
                msg.chat.id,
                "Отвечать (свайпать слево) можно только на входящие и исходящие сообщения.",
            )
            .reply_markup(KeyboardRemove::new())
            .await?;
        }
    } else {
        bot.send_message(msg.chat.id, format!("Кажется, вы отправили сообщение, но мы его не ждали... Может быть, \
        вы хотели отправить кому-то сообщение или ответить на полученное? В таком случае перейдите по ссылке друга или свайпните \
        полученное сообщение влево. А если вы хотите начать получать сообщения сами, то держите ссылку: {}", user_link.tme_url(&me)))
            .reply_markup(KeyboardRemove::new())
            .await?;
    }
    Ok(())
}

async fn handle_state_wait(
    db: Arc<Db>,
    bot: Bot,
    msg: Message,
    user_link: UserLink,
    me: Me,
    dialogue: MyDialogue,
    recipient_id: i64,
) -> Result<()> {
    if let Some(text) = msg.text()
        && text == "Отмена"
    {
        reset_dialogue(&bot, &dialogue, msg.chat.id).await?;
    } else if msg.reply_to_message().is_some() {
        bot.send_message(
            msg.chat.id,
            "Для отправки анонимного вопроса не нужно свайпать сообщение влево (отвечать). \
        Попробуйте ещё раз, отменив отправку вопроса или не отвечая на другие сообщения.",
        )
        .reply_markup(InlineKeyboardMarkup::new([[
            InlineKeyboardButton::callback("Отмена", "cancel"),
        ]]))
        .await?;
    } else {
        match forward_message(&bot, &db, &msg, ChatId(recipient_id), None).await {
            Ok(sent_msg_id) => {
                db.save_message(msg.chat.id.0, msg.id.0, recipient_id, sent_msg_id.0)
                    .await?;
                bot.send_message(
                    msg.chat.id,
                    format!(
                        "Ваше сообщение отправлено! А вот, кстати, ваша \
                        собственная ссылка для получения анонимных вопросов и сообщений: {}",
                        user_link.tme_url(&me)
                    ),
                )
                .reply_markup(KeyboardRemove::new())
                .await?;
            }
            Err(e) => {
                bot.send_message(
                    msg.chat.id,
                    format!(
                        "Не удалось отправить сообщение: {e}. Возможно, получатель заблокировал бота. \
                        А вот, кстати, ваша собственная ссылка для получения анонимных вопросов и сообщений: {}",
                        user_link.tme_url(&me)
                    ),
                )
                .reply_markup(KeyboardRemove::new())
                .await?;
            }
        }
        dialogue.reset().await?;
    }

    Ok(())
}

async fn handle_callback_query(
    db: Arc<Db>,
    bot: Bot,
    q: CallbackQuery,
    dialogue: MyDialogue,
) -> Result<()> {
    if let Some(data) = &q.data
        && let Some(chat_id) = q.chat_id()
    {
        match data.as_str() {
            "cancel" => {
                reset_dialogue(&bot, &dialogue, chat_id).await?;
                bot.edit_message_reply_markup(chat_id, q.message.context("no message")?.id)
                    .await?;
                bot.answer_callback_query(q.id).await?;
            }
            "reply" => {
                bot.answer_callback_query(q.id)
                    .cache_time(3600)
                    .show_alert(true)
                    .text("Для ответа используйте встроенную в Telegram функцию ответа на сообщение (свайпните влево)")
                    .await?;
                bot.send_message(chat_id, "Для ответа используйте встроенную в Telegram функцию ответа на сообщение (свайпните влево).\n\n\
                    Эта подсказка больше не будет отображаться.")
                    .await?;
                db.disable_answer_tip(chat_id.0).await?;
            }
            _ => {}
        }
    }
    Ok(())
}
