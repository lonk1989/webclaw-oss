mod builtin;
mod params;
mod parser;
mod skill_invoke;
mod store;

pub use params::{
    apply_recipe_params, apply_recipe_params_ex, collect_recipe_placeholder_names,
    extract_recipe_params_only, is_builtin_runtime_param, merge_params_into_content,
    merge_params_into_content_ex, ordered_vec_to_params, params_to_ordered_vec,
    parse_param_assignment, resolve_step_args, resolve_step_args_ex, strip_recipe_param_steps,
    substitute_recipe_params, substitute_recipe_params_ex, RecipeParamKv, RuntimeParamCtx,
};
pub use parser::{
    is_login_submit_click_target, norm_ui_contains, norm_ui_text, normalize_automation_text,
    parse_recipe_steps, OrBranch, OrGroupMeta, Step,
};
pub use skill_invoke::{
    apply_positional_params_to_skill_content, bind_skill_invocation_params, list_skill_invocations,
    resolve_skill_invocation, SkillInvocationInfo, SkillParamBinding,
};
pub use store::{
    delete_recipe, ensure_skill_executable, find_recipe_by_name, get_recipe, list_recipes,
    list_skills_cache, replace_skills_cache, save_recipe, with_skills_cache_exclusive, Recipe,
    RecipeSummary, SkillCacheEntry,
};
