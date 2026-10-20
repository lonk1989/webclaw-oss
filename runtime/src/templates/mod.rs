mod builtin;
mod store;

pub use store::{
    delete_template, ensure_template_files, get_by_id, import_template_file,
    list_templates, replace_template_file, reset_builtin_template, save_template,
    stage_template_file, sync_remote_templates, RemoteTemplateInput, templates_dir, TemplateDef,
};
