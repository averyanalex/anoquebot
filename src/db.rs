use anyhow::Result;
use entities::{messages, prelude::*, users};
use migration::{Migrator, MigratorTrait};
use rand::Rng;
use sea_orm::{prelude::*, ActiveValue, ConnectOptions, Database, DatabaseConnection, EntityTrait};
use tracing::log::LevelFilter;

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

    pub async fn get_user_link(&self, id: i64, invited_by: Option<i64>) -> Result<String> {
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
        Ok(link)
    }

    // pub async fn set_invited_by(&self, id: i64, invited_by: i64) -> Result<()> {
    //     Users::update_many()
    //         .col_expr(users::Column::InvitedBy, invited_by.into())
    //         .filter(users::Column::Id.eq(id))
    //         .filter(users::Column::InvitedBy.is_null())
    //         .exec(&self.dc)
    //         .await?;
    //     Ok(())
    // }

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

    pub async fn find_message(
        &self,
        recipient_id: i64,
        recipient_message_id: i32,
    ) -> Result<Option<(i64, i32)>> {
        let ids = Messages::find()
            .filter(messages::Column::RecipientId.eq(recipient_id))
            .filter(messages::Column::RecipientMessageId.eq(recipient_message_id))
            .one(&self.dc)
            .await?
            .map(|m| (m.sender_id, m.sender_message_id));
        Ok(ids)
    }
}
