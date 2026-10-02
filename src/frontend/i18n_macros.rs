/// Look up a translation key and return a reactive closure. Use inside `view! { ... }`.
///
///   view! { <button>{i18n!("login.submit")}</button> }
///   view! { <p>{i18n!("entry.confirm_delete", name = name.as_str())}</p> }
#[macro_export]
macro_rules! i18n {
    ($id:literal $(, $name:literal = $val:expr)* $(,)?) => {{
        let i18n = $crate::frontend::i18n::use_i18n();
        move || {
            #[allow(unused_mut)]
            let mut args: Vec<(&str, &str)> = Vec::new();
            $( args.push(($name, $val)); )*
            if args.is_empty() {
                i18n.t($id)
            } else {
                i18n.tf($id, &args)
            }
        }
    }};
}
