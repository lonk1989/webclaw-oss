mod args;
mod bitable_client;
mod bitable_store;
mod drive_client;
#[cfg(feature = "desktop")]
mod login;

pub use args::{
    field_summary, parse_bitable_query_args, parse_bitable_step_args, parse_bitable_step_args_allow_empty,
    parse_sync_args, query_conditions_summary, QueryCondition, QueryOp,
};
pub use bitable_client::{
    create_record, delete_records, format_records_markdown, read_excel_rows, records_to_plain_table,
    search_records, search_records_with_query, sync_records, table_field_order, upsert_record,
};
pub use bitable_store::{parse_bitable_url, resolve_for_command, BitableTable};
pub use drive_client::{
    download_file_by_token, parse_drive_folder_token, parse_open_api_file_token,
    upload_file_to_folder,
};

#[cfg(feature = "desktop")]
pub use login::{ensure_feishu_logged_in, url_needs_login_wait};

#[cfg(not(feature = "desktop"))]
pub fn url_needs_login_wait(_url: &str) -> bool {
    false
}
