use sea_orm_migration::prelude::*;

use crate::m20220101_000001_create_table::Users;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Messages::Table)
                    .col(
                        ColumnDef::new(Messages::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Messages::SenderId).big_integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Messages::Table, Messages::SenderId)
                            .to(Users::Table, Users::Id),
                    )
                    .col(
                        ColumnDef::new(Messages::SenderMessageId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Messages::RecipientId)
                            .big_integer()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Messages::Table, Messages::RecipientId)
                            .to(Users::Table, Users::Id),
                    )
                    .col(
                        ColumnDef::new(Messages::RecipientMessageId)
                            .integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Messages::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub enum Messages {
    Table,
    Id,
    SenderId,
    SenderMessageId,
    RecipientId,
    RecipientMessageId,
    Timestamp,
}
