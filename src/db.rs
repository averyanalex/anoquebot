use anyhow::{Context, Result};
use entities::{messages, prelude::*, users};
use migration::{Migrator, MigratorTrait, SimpleExpr};
use rand::Rng;
use sea_orm::{
    prelude::*, ActiveValue, ConnectOptions, Database, DatabaseConnection, EntityTrait,
    FromQueryResult, QuerySelect, SelectColumns,
};
use tracing::log::LevelFilter;

use crate::UserLink;

pub struct Db {
    dc: DatabaseConnection,
}

impl Db {
    pub async fn new() -> Result<Self> {
        let db_url = std::env::var("DATABASE_URL")?;

        let mut conn_options = ConnectOptions::new(db_url);
        conn_options.sqlx_logging_level(LevelFilter::Debug);
        conn_options.sqlx_logging(true);

        let dc = Database::connect(conn_options).await?;
        Migrator::up(&dc, None).await?;
        Ok(Self { dc })
    }

    pub async fn get_user_link(&self, id: i64, invited_by: Option<i64>) -> Result<UserLink> {
        let link = if let Some(user) = Users::find_by_id(id).one(&self.dc).await? {
            Users::update_many()
                .col_expr(
                    users::Column::LastActivity,
                    Expr::current_timestamp().into(),
                )
                .filter(users::Column::Id.eq(id))
                .exec(&self.dc)
                .await?;
            user.link
        } else {
            let link: String = {
                const CHARSET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
                let mut rng = rand::thread_rng();
                (0..8)
                    .map(|_| {
                        let idx = rng.gen_range(0..CHARSET.len());
                        CHARSET[idx] as char
                    })
                    .collect()
            };

            let user = users::ActiveModel {
                id: ActiveValue::Set(id),
                link: ActiveValue::Set(link.clone()),
                invited_by: ActiveValue::Set(invited_by),
                ..Default::default()
            };
            Users::insert(user).exec(&self.dc).await?;
            link
        };
        Ok(UserLink(link))
    }

    pub async fn user_id_by_link(&self, link: &str) -> Result<Option<i64>> {
        let id = Users::find()
            .filter(users::Column::Link.eq(link))
            .one(&self.dc)
            .await?
            .map(|u| u.id);
        Ok(id)
    }

    pub async fn save_message(
        &self,
        sender_id: i64,
        sender_message_id: i32,
        recipient_id: i64,
        recipient_message_id: i32,
    ) -> Result<()> {
        let message = messages::ActiveModel {
            sender_id: ActiveValue::Set(sender_id),
            sender_message_id: ActiveValue::Set(sender_message_id),
            recipient_id: ActiveValue::Set(recipient_id),
            recipient_message_id: ActiveValue::Set(recipient_message_id),
            ..Default::default()
        };
        Messages::insert(message).exec(&self.dc).await?;
        Ok(())
    }

    pub async fn find_another_message(
        &self,
        chat_id: i64,
        msg_id: i32,
    ) -> Result<Option<(i64, i32)>> {
        let ids = Messages::find()
            .filter(SimpleExpr::or(
                messages::Column::RecipientId
                    .eq(chat_id)
                    .and(messages::Column::RecipientMessageId.eq(msg_id)),
                messages::Column::SenderId
                    .eq(chat_id)
                    .and(messages::Column::SenderMessageId.eq(msg_id)),
            ))
            .one(&self.dc)
            .await?;
        Ok(if let Some(ids) = ids {
            if chat_id == ids.recipient_id && msg_id == ids.recipient_message_id {
                Some((ids.sender_id, ids.sender_message_id))
            } else {
                Some((ids.recipient_id, ids.recipient_message_id))
            }
        } else {
            None
        })
    }

    pub async fn disable_answer_tip(&self, user_id: i64) -> Result<()> {
        Users::update_many()
            .col_expr(users::Column::AnswerTip, Expr::value(false))
            .filter(users::Column::Id.eq(user_id))
            .exec(&self.dc)
            .await?;
        Ok(())
    }

    pub async fn answer_tip_enabled(&self, user_id: i64) -> Result<bool> {
        let user = Users::find_by_id(user_id)
            .one(&self.dc)
            .await?
            .context("user not found")?;
        Ok(user.answer_tip)
    }

    pub async fn get_all_users(&self) -> Result<Vec<i64>> {
        #[derive(FromQueryResult)]
        struct UserWithId {
            id: i64,
        }

        let users = Users::find()
            .select_only()
            .select_column(users::Column::Id)
            .into_model::<UserWithId>()
            .all(&self.dc)
            .await?;

        Ok(users.into_iter().map(|u| u.id).collect())
    }
}
