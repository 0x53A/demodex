mod app;
mod approval;
mod client;
mod sync;
mod controls;
mod conversation;
mod diff;
mod directory_picker;
mod modal;
mod model;
mod music;
mod notifications;
mod overview;
mod reorder;
mod rich_messages;
mod links;
mod images;
mod svg;
mod runtime_features;
mod prompt_settings;
mod transcript;
mod timestamps;
mod usage;
mod ui;

fn main() {
    console_error_panic_hook::set_once();
    wasm_bindgen_futures::spawn_local(async {
        let window = web_sys::window().unwrap();
        let ready = js_sys::Reflect::get(&window, &"demodexPwaReady".into()).ok();
        if let Some(value) = ready.filter(|v| !v.is_undefined()) {
            let promise = js_sys::Promise::resolve(&value);
            if let Ok(result) = wasm_bindgen_futures::JsFuture::from(promise).await
                && result.as_bool() == Some(false)
            {
                return;
            }
        }
        let root = window.document().unwrap().get_element_by_id("app").unwrap();
        root.set_text_content(None);
        yew::Renderer::<app::App>::with_root(root).render();
    });
}
