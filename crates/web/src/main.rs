mod app;
mod client;
mod controls;
mod conversation;
mod modal;
mod model;
mod overview;
mod transcript;
mod usage;

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
