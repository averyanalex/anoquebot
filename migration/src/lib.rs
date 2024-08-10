pub use sea_orm_migration::prelude::*;

mod m20220101_000001_create_table;
mod m20240129_132329_create_messages;
mod m20240129_173538_add_timestamps;
mod m20240720_120000_add_answer_tip_field;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20220101_000001_create_table::Migration),
            Box::new(m20240129_132329_create_messages::Migration),
            Box::new(m20240129_173538_add_timestamps::Migration),
            Box::new(m20240720_120000_add_answer_tip_field::Migration),
        ]
    }
}
