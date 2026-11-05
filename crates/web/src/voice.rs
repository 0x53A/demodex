use crate::client::Client;
use demodex_protocol::{VoiceCommand as Command, VoiceReply};
use std::{cell::{Cell, RefCell}, rc::Rc};
use tokio::sync::mpsc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use yew::prelude::*;
#[wasm_bindgen(module = "/src/voice.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = prepareRecording)] async fn prepare(callback: &js_sys::Function, warning: &js_sys::Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = startRecording)] fn start(controller: &JsValue) -> Result<(), JsValue>;
    #[wasm_bindgen(js_name = stopRecording)] fn stop(controller: &JsValue) -> js_sys::Promise;
    #[wasm_bindgen(js_name = cancelRecording)] fn cancel(controller: &JsValue);
}
enum Input { Audio(Vec<i16>), Finish, Error(String) }
struct Capture {
    controller: RefCell<Option<JsValue>>,
    alive: Cell<bool>,
    tx: mpsc::Sender<Input>,
    callback: Closure<dyn Fn(JsValue)>,
    warning: Closure<dyn Fn(String)>,
}
impl Capture {
    fn cancel(&self) {
        self.alive.set(false);
        if let Some(c) = self.controller.borrow_mut().take() { cancel(&c); }
    }
}
impl Drop for Capture { fn drop(&mut self) { self.cancel(); } }
#[derive(Properties)]
pub struct Props {
    pub client: Rc<Client>,
    pub session: String,
    pub ontext: Callback<String>,
    pub onbusy: Callback<bool>,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool { Rc::ptr_eq(&self.client, &other.client) && self.session == other.session && self.ontext == other.ontext && self.onbusy == other.onbusy }
}
pub struct Voice {
    capture: Option<Rc<Capture>>,
    id: String,
    language: String,
    phase: &'static str,
    error: String,
}
pub enum Msg { Record, Stop, Cancel, Language(String), Ready(String), Finishing(String), Warning(String, String), Done(String, Result<String, String>) }
impl Voice {
    fn cancel(&mut self, ctx: &Context<Self>) {
        if let Some(capture) = self.capture.take() { capture.cancel(); }
        if !self.id.is_empty() {
            let client = ctx.props().client.clone(); let id = std::mem::take(&mut self.id);
            spawn_local(async move { let _ = client.voice(Command::Cancel { id }).await; });
        }
        self.phase = "";
        ctx.props().onbusy.emit(false);
    }
}
impl Component for Voice {
    type Message = Msg;
    type Properties = Props;
    fn create(_: &Context<Self>) -> Self { Self { capture: None, id: String::new(), language: "en".into(), phase: "", error: String::new() } }
    fn destroy(&mut self, ctx: &Context<Self>) { self.cancel(ctx); }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Language(s) => self.language = s,
            Msg::Cancel => self.cancel(ctx),
            Msg::Finishing(id) => { if self.id == id { self.phase = "Transcribing…"; } }
            Msg::Ready(id) => { if self.id == id { self.phase = "Recording…"; } }
            Msg::Warning(id, warning) => { if self.id == id { self.error = warning; } }
            Msg::Done(id, result) => {
                if self.id != id { return false; }
                self.cancel(ctx);
                match result {
                    Ok(text) if !text.trim().is_empty() => ctx.props().ontext.emit(text),
                    Ok(_) => self.error = "No speech recognized. Try again.".into(),
                    Err(error) => self.error = error,
                }
            }
            Msg::Stop => {
                if self.phase != "Recording…" { return false; }
                self.phase = "Transcribing…";
                if let Some(capture) = self.capture.clone() {
                    let controller = capture.controller.borrow().clone();
                    if let Some(controller) = controller {
                        spawn_local(async move {
                            let _ = JsFuture::from(stop(&controller)).await;
                            if capture.alive.get() { let _ = capture.tx.send(Input::Finish).await; }
                        });
                    }
                }
            }
            Msg::Record => {
                if !self.id.is_empty() { return false; }
                self.id = uuid::Uuid::new_v4().to_string();
                self.error.clear(); self.phase = "Preparing microphone…";
                ctx.props().onbusy.emit(true);
                let (tx, mut rx) = mpsc::channel(8);
                let cbtx = tx.clone();
                let failure = Rc::new(RefCell::new(None::<String>));
                let cbfailure = failure.clone();
                let callback = Closure::wrap(Box::new(move |data: JsValue| {
                    let input = if let Some(error) = data.as_string() { if error == "finish" {Input::Finish} else {Input::Error(error)} } else { Input::Audio(js_sys::Int16Array::new(&data).to_vec()) };
                    if cbtx.try_send(input).is_err() { *cbfailure.borrow_mut() = Some("Audio connection is too slow; recording cancelled".into()); }
                }) as Box<dyn Fn(JsValue)>);
                let warning_id = self.id.clone();
                let warning_link = ctx.link().clone();
                let warning = Closure::wrap(Box::new(move |text: String| {
                    warning_link.send_message(Msg::Warning(warning_id.clone(), text));
                }) as Box<dyn Fn(String)>);
                let capture = Rc::new(Capture { controller: RefCell::new(None), alive: Cell::new(true), tx, callback, warning });
                self.capture = Some(capture.clone());
                let id = self.id.clone(); let language = self.language.clone();
                let client = ctx.props().client.clone(); let link = ctx.link().clone();
                let session = ctx.props().session.clone();
                spawn_local(async move {
                    let result = async {
                        let controller = prepare(capture.callback.as_ref().unchecked_ref(), capture.warning.as_ref().unchecked_ref()).await.map_err(js_error)?;
                        if !capture.alive.get() { cancel(&controller); return Err("Recording cancelled".into()); }
                        *capture.controller.borrow_mut() = Some(controller.clone());
                        client.voice(Command::Start { id: id.clone(), language, session }).await.map_err(|e|e.to_string())?;
                        if !capture.alive.get() { return Err("Recording cancelled".into()); }
                        start(&controller).map_err(js_error)?;
                        link.send_message(Msg::Ready(id.clone()));
                        let mut sequence = 0;
                        loop {
                            if !capture.alive.get() { return Err("Recording cancelled".into()); }
                            if let Some(error) = failure.borrow_mut().take() { return Err(error); }
                            // A short tick lets cancellation tear down the task even with no audio.
                            let event = futures_util::future::select(Box::pin(rx.recv()), Box::pin(gloo::timers::future::TimeoutFuture::new(250))).await;
                            let input = match event { futures_util::future::Either::Left((Some(input), _)) => input, _ => continue };
                            match input {
                                Input::Audio(pcm) => {
                                    client.voice(Command::Audio { id: id.clone(), sequence, pcm }).await.map_err(|e|e.to_string())?;
                                    sequence += 1;
                                }
                                Input::Finish => {
                                    link.send_message(Msg::Finishing(id.clone()));
                                    return match client.voice(Command::Finish {id: id.clone()}).await.map_err(|e|e.to_string())? {
                                        VoiceReply::Text(text) => Ok(text), _ => Err("Invalid transcription response".into()),
                                    };
                                }
                                Input::Error(error) => return Err(error),
                            }
                        }
                    }.await;
                    capture.cancel();
                    let _ = client.voice(Command::Cancel { id: id.clone() }).await;
                    link.send_message(Msg::Done(id, result));
                });
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let idle = self.id.is_empty();
        let recording = self.phase == "Recording…";
        html! {<span class="voice-input">
            <span class="voice-split">
                <button type="button" class={classes!("voice-record", (!idle).then_some("active"))}
                    aria-label={if idle {"Record voice"} else {"Stop"}}
                    title={if idle {"Record voice"} else {"Stop recording and transcribe"}}
                    disabled={!idle && !recording}
                    onclick={ctx.link().callback(move |_| if idle {Msg::Record} else {Msg::Stop})}>
                    {if idle {html!{
                        <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
                            <rect x="9" y="2" width="6" height="12" rx="3"/>
                            <path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8"/>
                        </svg>
                    }} else {html!{
                        <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor" aria-hidden="true"><rect x="5" y="5" width="14" height="14"/></svg>
                    }}}
                </button>
                <select aria-label="Speech language" title="Speech language" disabled={!idle} value={self.language.clone()} onchange={ctx.link().callback(|e: Event| Msg::Language(e.target_unchecked_into::<web_sys::HtmlSelectElement>().value()))}>
                    <option value="en">{"EN"}</option><option value="de">{"DE"}</option>
                </select>
            </span>
            {if idle {Html::default()} else {html!{<span class="voice-progress">
                <span role="status">{self.phase}</span>
                <button type="button" onclick={ctx.link().callback(|_|Msg::Cancel)}>{"Cancel"}</button>
            </span>}}}
            {if self.error.is_empty() {Html::default()} else {html!{<span class="error" role="alert">{&self.error}</span>}}}
        </span>}
    }
}
fn js_error(error: JsValue) -> String {
    error.as_string().or_else(|| js_sys::Reflect::get(&error, &"message".into()).ok().and_then(|v|v.as_string())).unwrap_or_else(||"Microphone unavailable or permission denied".into())
}

/// Preserve tag boundaries even if the recognizer emits markup-like text.
pub fn tag_transcript(text: &str) -> String {
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("<tts>\n{escaped}\n</tts>")
}
#[cfg(test)]
mod tests {
    #[test]
    fn dictation_is_delimited_without_rewriting_words() {
        assert_eq!(super::tag_transcript("we was thinking"), "<tts>\nwe was thinking\n</tts>");
        assert_eq!(super::tag_transcript("a </tts> & b"), "<tts>\na &lt;/tts&gt; &amp; b\n</tts>");
    }
}
