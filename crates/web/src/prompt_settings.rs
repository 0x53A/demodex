use crate::{
    client::Client,
    model::{array, text},
};
use demodex_protocol::{ModelPromptOverride, Operation, PromptSettings as Settings, Selection};
use serde_json::Value;
use std::rc::Rc;
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

#[derive(Properties)]
pub struct Props {
    pub client: Rc<Client>,
}
impl PartialEq for Props {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.client, &o.client)
    }
}
struct Editor {
    model: Option<String>,
    baseline: String,
    fingerprint: String,
    draft: String,
    diff: bool,
}
pub struct PromptSettings {
    data: Value,
    settings: Settings,
    editor: Option<Editor>,
    busy: bool,
    error: String,
    serial: u64,
    dirty: bool,
}
pub enum Msg {
    Load,
    Loaded(u64, Result<Value, String>),
    Append(String),
    Project(bool),
    Add,
    Model(String, String),
    Edit(Option<String>),
    Remove(String),
    Restore,
    Draft(String),
    Diff(bool),
    Close,
    Commit,
    Save,
    Saved(u64, Result<Value, String>),
}
impl Component for PromptSettings {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Load);
        Self {
            data: Value::Null,
            settings: Settings::default(),
            editor: None,
            busy: false,
            error: String::new(),
            serial: 0,
            dirty: false,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Props) -> bool {
        self.serial += 1;
        self.editor = None;
        self.data = Value::Null;
        self.dirty = false;
        ctx.link().send_message(Msg::Load);
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Load => {
                if self.busy {
                    return false;
                }
                self.serial += 1;
                self.busy = true;
                self.error.clear();
                let serial = self.serial;
                let client = ctx.props().client.clone();
                ctx.link().send_future(async move {
                    Msg::Loaded(
                        serial,
                        client
                            .read(Operation::PromptSettings)
                            .await
                            .map_err(|e| format!("{e:#}")),
                    )
                });
            }
            Msg::Loaded(serial, result) => {
                if serial != self.serial {
                    return false;
                }
                self.busy = false;
                match result {
                    Ok(data) => match serde_json::from_value(data["settings"].clone()) {
                        Ok(settings) => {
                            self.settings = settings;
                            self.data = data;
                            self.dirty = false;
                        }
                        Err(e) => self.error = e.to_string(),
                    },
                    Err(e) => self.error = e,
                }
            }
            Msg::Append(v) => {
                self.settings.append = v;
                self.dirty = true;
            }
            Msg::Project(v) => {
                self.settings.include_project = v;
                self.dirty = true;
            }
            Msg::Add => {
                if let Some(model) = array(&self.data["models"])
                    .iter()
                    .map(|m| text(m, "model"))
                    .find(|m| {
                        !self.settings.models.contains_key(*m)
                            && self.data["defaults"][*m]["text"].is_string()
                    })
                {
                    let model = model.to_owned();
                    self.settings.models.insert(
                        model.clone(),
                        ModelPromptOverride {
                            text: text(&self.data["defaults"][&model], "effective_text").into(),
                            reviewed_default: text(&self.data["defaults"][&model], "fingerprint")
                                .into(),
                        },
                    );
                    self.dirty = true;
                }
            }
            Msg::Model(old, new) => {
                if old != new && !self.settings.models.contains_key(&new) {
                    self.settings.models.remove(&old);
                    self.settings.models.insert(
                        new.clone(),
                        ModelPromptOverride {
                            text: text(&self.data["defaults"][&new], "effective_text").into(),
                            reviewed_default: text(&self.data["defaults"][&new], "fingerprint")
                                .into(),
                        },
                    );
                    self.dirty = true;
                }
            }
            Msg::Edit(model) => {
                let baseline = if let Some(m) = &model {
                    &self.data["defaults"][m]
                } else {
                    &self.data["integration_default"]
                };
                if !baseline["text"].is_string() {
                    self.error = "Default prompt is unavailable; refresh the catalogue.".into();
                    return true;
                }
                let draft = model
                    .as_ref()
                    .and_then(|m| self.settings.models.get(m))
                    .or_else(|| {
                        if model.is_none() {
                            self.settings.integration.as_ref()
                        } else {
                            None
                        }
                    })
                    .map(|v| v.text.clone())
                    .unwrap_or_else(|| text(baseline, "text").into());
                self.editor = Some(Editor {
                    model,
                    baseline: text(baseline, "text").into(),
                    fingerprint: text(baseline, "fingerprint").into(),
                    draft,
                    diff: false,
                });
            }
            Msg::Remove(m) => {
                self.settings.models.remove(&m);
                self.dirty = true;
            }
            Msg::Restore => {
                if let Some(e) = &mut self.editor {
                    e.draft = e.baseline.clone();
                }
            }
            Msg::Draft(v) => {
                if let Some(e) = &mut self.editor {
                    e.draft = v;
                }
            }
            Msg::Diff(v) => {
                if let Some(e) = &mut self.editor {
                    e.diff = v;
                }
            }
            Msg::Close => self.editor = None,
            Msg::Commit => {
                if let Some(e) = self.editor.take() {
                    let entry = ModelPromptOverride {
                        text: e.draft,
                        reviewed_default: e.fingerprint,
                    };
                    if let Some(m) = e.model {
                        self.settings.models.insert(m, entry);
                    } else {
                        self.settings.integration = if entry.text == e.baseline {
                            None
                        } else {
                            Some(entry)
                        };
                    }
                    self.dirty = true;
                }
                ctx.link().send_message(Msg::Save);
            }

            Msg::Save => {
                if self.busy || self.data.is_null() {
                    return false;
                }
                self.serial += 1;
                let serial = self.serial;
                self.busy = true;
                self.error.clear();
                let client = ctx.props().client.clone();
                let op = Operation::SavePromptSettings {
                    expected_revision: self.data["revision"].as_u64().unwrap_or(0),
                    settings: self.settings.clone(),
                };
                let receipt = uuid::Uuid::new_v4().to_string();
                ctx.link().send_future(async move {
                    Msg::Saved(
                        serial,
                        client.call(op, receipt.clone()).await.map_err(|e| {
                            format!(
                                "{e:#} · Receipt: {receipt}. Changes are not retried automatically."
                            )
                        }),
                    )
                });
            }
            Msg::Saved(serial, result) => {
                if serial != self.serial {
                    return false;
                }
                self.busy = false;
                match result {
                    Ok(v) => {
                        self.data["revision"] = v["revision"].clone();
                        self.dirty = false;
                    }
                    Err(e) => self.error = e,
                }
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {<crate::ui::Group title="System prompts" class="prompt-settings">
         <p class="muted">{"Saved prompts apply to new sessions and explicit reconnects. Existing sessions can apply them from Session controls. Files in the Codex profile are never modified."}</p>
         <button type="button" disabled={self.busy||self.dirty} onclick={ctx.link().callback(|_|Msg::Load)}>{"Refresh Codex defaults"}</button>
         {if self.busy{html!{<p role="status">{"Loading or saving prompt settings…"}</p>}}else{Html::default()}}
         {if !self.error.is_empty(){html!{<><p role="alert" class="error">{&self.error}</p><button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Load)}>{"Reload saved settings"}</button></>}}else{Html::default()}}
         <fieldset disabled={self.busy||self.data.is_null()}>
         <crate::ui::SectionTitle>{"Replace the system prompt for specific models"}</crate::ui::SectionTitle>
         {for self.settings.models.iter().map(|(model,entry)|{let m=model.clone();let edit=model.clone();let remove=model.clone();let changed=self.data["defaults"][model]["fingerprint"].as_str().is_some_and(|h|h!=entry.reviewed_default);html!{<div class="prompt-model-row">
          <select aria-label="Prompt model" value={model.clone()} onchange={ctx.link().callback(move |e:Event|Msg::Model(m.clone(),e.target_unchecked_into::<HtmlSelectElement>().value()))}>
           {if !array(&self.data["models"]).iter().any(|m|m["model"]==*model){html!{<option value={model.clone()}>{format!("{model} (unavailable)")}</option>}}else{Html::default()}}
           {for array(&self.data["models"]).iter().map(|m|{let slug=text(m,"model");html!{<option value={slug.to_owned()} selected={slug==model} disabled={slug!=model&&self.settings.models.contains_key(slug)}>{slug}</option>}})}
          </select>
          <button type="button" aria-label={format!("Edit prompt for {model}")} title="Edit prompt" onclick={ctx.link().callback(move |_|Msg::Edit(Some(edit.clone())))}>{"✎"}</button>
          <button type="button" aria-label={format!("Remove prompt for {model}")} title="Restore Codex default" onclick={ctx.link().callback(move |_|Msg::Remove(remove.clone()))}>{"×"}</button>
          {if changed{html!{<span class="control-warning" role="status">{"Codex default changed — review diff"}</span>}}else{Html::default()}}
          {if let Some(error)=self.data["defaults"][model]["error"].as_str(){html!{<span class="error">{error}</span>}}else{Html::default()}}
         </div>}})}
         <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::Add)}>{"+ Add model override"}</crate::ui::AddButton>
         <label>{"Append instructions for all models"}<textarea aria-label="Append instructions for all models" rows="5" value={self.settings.append.clone()} oninput={ctx.link().callback(|e:InputEvent|Msg::Append(e.target_unchecked_into::<HtmlTextAreaElement>().value()))}/></label>
         <p class="muted">{"Appended after either the Codex default or your replacement."}</p>
         <div class="prompt-model-row"><span>{"Demodex integration instructions"}</span><button type="button" aria-label="Edit Demodex integration instructions" title="Edit integration instructions" onclick={ctx.link().callback(|_|Msg::Edit(None))}>{"✎"}</button>
         {if self.settings.integration.as_ref().is_some_and(|v|v.reviewed_default!=text(&self.data["integration_default"],"fingerprint")){html!{<span role="status" class="control-warning">{"Demodex default changed — review diff"}</span>}}else{Html::default()}}</div>
         <label class="checkbox"><input type="checkbox" checked={self.settings.include_project} onchange={ctx.link().callback(|e:Event|Msg::Project(e.target_unchecked_into::<HtmlInputElement>().checked()))}/>{"Include project instruction files by default"}</label>
         <InstructionPreview client={ctx.props().client.clone()} target={None::<Selection>} global_inline=true/>
         <button type="button" class="primary" disabled={!self.dirty} onclick={ctx.link().callback(|_|Msg::Save)}>{"Save prompt settings"}</button>
         </fieldset>
         {if let Some(e)=&self.editor{let source=e.model.as_ref().map(|m|text(&self.data["defaults"][m],"source")).unwrap_or("Installed Demodex");html!{<crate::modal::Modal title={e.model.as_ref().map(|m|format!("System prompt · {m}")).unwrap_or("Demodex integration instructions".into())} onclose={ctx.link().callback(|_|Msg::Close)}>
          <p class="muted">{format!("Default source: {source}. The comparison uses the latest fetched default.")}</p>
          {if let Some(m)=&e.model{let d=&self.data["defaults"][m];if d["effective_source"]!=d["source"]{html!{<><p class="control-warning">{format!("Codex profile currently overrides this default via {}.",text(d,"effective_source"))}</p><details><summary>{"Current Codex profile prompt (read-only)"}</summary><pre class="instruction-file">{text(d,"effective_text")}</pre></details></>}}else{Html::default()}}else{Html::default()}}
          <div class="prompt-editor-actions"><button type="button" aria-pressed={(!e.diff).to_string()} onclick={ctx.link().callback(|_|Msg::Diff(false))}>{"Edit"}</button><button type="button" aria-pressed={e.diff.to_string()} onclick={ctx.link().callback(|_|Msg::Diff(true))}>{"Diff"}</button><button type="button" onclick={ctx.link().callback(|_|Msg::Restore)}>{"Reset to current default"}</button></div>
          <PromptEditor baseline={e.baseline.clone()} value={e.draft.clone()} diff={e.diff} oninput={ctx.link().callback(Msg::Draft)}/>
          <div class="prompt-editor-actions"><button type="button" onclick={ctx.link().callback(|_|Msg::Close)}>{"Cancel"}</button><button type="button" class="primary" onclick={ctx.link().callback(|_|Msg::Commit)}>{"Save"}</button></div>
         </crate::modal::Modal>}}else{Html::default()}}
        </crate::ui::Group>}
    }
}

#[derive(Properties, PartialEq)]
pub struct EditorProps {
    pub baseline: String,
    pub value: String,
    pub diff: bool,
    pub oninput: Callback<String>,
}
#[function_component(PromptEditor)]
fn prompt_editor(props: &EditorProps) -> Html {
    let wrapping = use_state(|| true);
    let set_wrapping = wrapping.clone();
    let onwrap = Callback::from(move |event: Event| {
        set_wrapping.set(event.target_unchecked_into::<HtmlInputElement>().checked());
    });
    let overlay = use_node_ref();
    let target = overlay.clone();
    let onscroll = Callback::from(move |event: Event| {
        let input = event.target_unchecked_into::<HtmlTextAreaElement>();
        if let Some(pre) = target.cast::<web_sys::HtmlElement>() {
            pre.set_scroll_top(input.scroll_top());
            pre.set_scroll_left(input.scroll_left());
        }
    });
    let (old, new) = changed_lines(&props.baseline, &props.value);
    let (old_words, new_words) = if props.diff {
        changed_words(&props.baseline, &props.value, &old, &new)
    } else {
        (Vec::new(), Vec::new())
    };
    let digits = props
        .value
        .split('\n')
        .count()
        .max(props.baseline.split('\n').count())
        .to_string()
        .len();
    let numbered_lines =
        |source: &str, changed: &[bool], words: &[Vec<(&str, bool)>], class: &'static str| {
            source.split('\n').enumerate().map(|(i, line)| html! {
            <span class={classes!("prompt-line", changed[i].then_some(class))}>
                <span class="prompt-line-number" aria-hidden="true">{i + 1}</span>
                {if line.is_empty() {html!{"\u{200b}"}} else if props.diff {
                    html!{{for words[i].iter().map(|(word, changed)| html!{
                        <span class={if *changed {"diff-word"} else {"diff-context"}}>{*word}</span>
                    })}}
                } else {html!{line}}}
            </span>
        }).collect::<Html>()
        };
    html! {<div class={classes!("prompt-editor",props.diff.then_some("prompt-diff"),(props.diff && !*wrapping).then_some("prompt-nowrap"))} style={format!("--prompt-gutter:{}ch",digits + 2)}>
    {if props.diff {html!{<><div class="prompt-diff-options"><label class="checkbox"><input type="checkbox" checked={*wrapping} onchange={onwrap}/>{"Word wrapping"}</label><span class="muted">{"Removed words are struck through; added words are underlined. Unchanged text stays neutral."}</span></div><div class="prompt-original"><strong>{"Current default"}</strong><pre>{numbered_lines(&props.baseline, &old, &old_words, "diff-delete")}</pre></div></>}}else{Html::default()}}
    <div class="prompt-replacement"><strong>{"Your version"}</strong><div class="prompt-edit-area">
    <pre ref={overlay} aria-hidden="true">{numbered_lines(&props.value, &new, &new_words, "diff-add")}</pre>
    <textarea aria-label="Prompt text" spellcheck="false" wrap={if props.diff && !*wrapping {"off"} else {"soft"}} value={props.value.clone()} {onscroll} oninput={props.oninput.reform(|e:InputEvent|e.target_unchecked_into::<HtmlTextAreaElement>().value())}/>
    </div></div></div>}
}
fn changed_lines(old: &str, new: &str) -> (Vec<bool>, Vec<bool>) {
    let a: Vec<_> = old.split('\n').collect();
    let b: Vec<_> = new.split('\n').collect();
    changed_items(&a, &b)
}
fn changed_items(a: &[&str], b: &[&str]) -> (Vec<bool>, Vec<bool>) {
    let mut left = vec![true; a.len()];
    let mut right = vec![true; b.len()];
    if a.len().saturating_mul(b.len()) > 2_000_000 {
        let mut j = 0;
        for (i, line) in a.iter().enumerate() {
            if let Some(k) = b[j..].iter().position(|v| v == line) {
                j += k;
                left[i] = false;
                right[j] = false;
                j += 1;
            }
        }
        return (left, right);
    }
    let width = b.len() + 1;
    let mut lengths = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lengths[i * width + j] = if a[i] == b[j] {
                1 + lengths[(i + 1) * width + j + 1]
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            left[i] = false;
            right[j] = false;
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (left, right)
}

// Compare words inside changed blocks between matching source lines. This keeps
// repeated words elsewhere in a long prompt from stealing local matches.
type WordLines<'a> = Vec<Vec<(&'a str, bool)>>;
fn changed_words<'a>(
    old: &'a str,
    new: &'a str,
    left: &[bool],
    right: &[bool],
) -> (WordLines<'a>, WordLines<'a>) {
    fn words(source: &str) -> WordLines<'_> {
        source
            .split('\n')
            .map(|line| {
                let mut tokens = Vec::new();
                let mut start = 0;
                let mut previous = None;
                for (i, ch) in line.char_indices() {
                    let kind = if ch.is_whitespace() {
                        0
                    } else if ch.is_alphanumeric() || ch == '_' {
                        1
                    } else {
                        2
                    };
                    if previous.is_some_and(|p| p != kind || kind == 2) {
                        tokens.push((&line[start..i], false));
                        start = i;
                    }
                    previous = Some(kind);
                }
                if start < line.len() {
                    tokens.push((&line[start..], false));
                }
                tokens
            })
            .collect()
    }
    let (mut a, mut b) = (words(old), words(new));
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let (start_i, start_j) = (i, j);
        while i < a.len() && left[i] {
            i += 1;
        }
        while j < b.len() && right[j] {
            j += 1;
        }
        let old_tokens: Vec<_> = a[start_i..i]
            .iter()
            .flatten()
            .map(|(word, _)| *word)
            .collect();
        let new_tokens: Vec<_> = b[start_j..j]
            .iter()
            .flatten()
            .map(|(word, _)| *word)
            .collect();
        let (removed, added) = changed_items(&old_tokens, &new_tokens);
        for (token, changed) in a[start_i..i].iter_mut().flatten().zip(removed) {
            token.1 = changed;
        }
        for (token, changed) in b[start_j..j].iter_mut().flatten().zip(added) {
            token.1 = changed;
        }
        // The next unchanged line is a shared anchor, if present.
        if i < a.len() {
            i += 1;
        }
        if j < b.len() {
            j += 1;
        }
    }
    (a, b)
}

#[derive(Properties)]
pub struct PreviewProps {
    pub client: Rc<Client>,
    pub target: Option<Selection>,
    #[prop_or_default]
    pub global_inline: bool,
}
impl PartialEq for PreviewProps {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.client, &o.client)
            && self.target == o.target
            && self.global_inline == o.global_inline
    }
}
pub struct InstructionPreview {
    open: bool,
    data: Value,
    error: String,
    serial: u64,
    busy: bool,
}
pub enum PreviewMsg {
    Open,
    Close,
    Loaded(u64, Result<Value, String>),
}
impl Component for InstructionPreview {
    type Message = PreviewMsg;
    type Properties = PreviewProps;
    fn create(ctx: &Context<Self>) -> Self {
        if ctx.props().global_inline {
            ctx.link().send_message(PreviewMsg::Open);
        }
        Self {
            open: false,
            data: Value::Null,
            error: String::new(),
            serial: 0,
            busy: false,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &PreviewProps) -> bool {
        self.open = false;
        self.serial += 1;
        self.data = Value::Null;
        self.error.clear();
        if ctx.props().global_inline {
            ctx.link().send_message(PreviewMsg::Open);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: PreviewMsg) -> bool {
        match msg {
            PreviewMsg::Open => {
                self.open = true;
                self.busy = true;
                self.data = Value::Null;
                self.error.clear();
                self.serial += 1;
                let serial = self.serial;
                let client = ctx.props().client.clone();
                let target = ctx.props().target.clone();
                ctx.link().send_future(async move {
                    PreviewMsg::Loaded(
                        serial,
                        client
                            .read(Operation::InstructionFiles { target })
                            .await
                            .map_err(|e| format!("{e:#}")),
                    )
                });
            }
            PreviewMsg::Close => {
                self.open = false;
                self.serial += 1;
            }
            PreviewMsg::Loaded(serial, result) => {
                if serial != self.serial || !self.open {
                    return false;
                }
                self.busy = false;
                match result {
                    Ok(v) => self.data = v,
                    Err(e) => self.error = e,
                }
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        if ctx.props().global_inline {
            return html! {<>
                {if !self.error.is_empty(){html!{<p class="error" role="alert">{format!("Could not read global instructions: {}",self.error)}</p>}}else{Html::default()}}
                {for array(&self.data["files"]).iter().filter(|file|file["scope"]=="global").map(|file|html!{
                    <details><summary>{format!("Global Codex instructions · {}",text(file,"path"))}</summary>
                    <p class="muted">{"Codex loads these instructions automatically. This preview is read-only."}</p>
                    <pre class="instruction-file">{text(file,"text")}</pre></details>
                })}
            </>};
        }
        html! {<><button type="button" onclick={ctx.link().callback(|_|PreviewMsg::Open)}>{"Read instruction files"}</button>{if self.open{html!{<crate::modal::Modal title="Instruction files (read-only)" onclose={ctx.link().callback(|_|PreviewMsg::Close)}>
        {if self.busy{html!{<p role="status">{"Reading instruction files…"}</p>}}else{Html::default()}}
        <p class="muted">{text(&self.data,"note")}</p>{if !self.error.is_empty(){html!{<p class="error" role="alert">{&self.error}</p>}}else{Html::default()}}
        {for array(&self.data["files"]).iter().map(|file|html!{<details><summary>{format!("{} · {}",text(file,"scope"),text(file,"path"))}</summary>{if file["truncated"]==true{html!{<p class="control-warning">{"Codex's project instruction limit excludes some or all of this file."}</p>}}else{Html::default()}}<pre class="instruction-file">{text(file,"text")}</pre></details>})}
        {if !self.busy&&self.error.is_empty()&&array(&self.data["files"]).is_empty(){html!{<p>{"No instruction files found."}</p>}}else{Html::default()}}
        </crate::modal::Modal>}}else{Html::default()}}</>}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn word_diff_preserves_context_punctuation_unicode_and_blank_lines() {
        let old = "Keep the red café.\n\nUnchanged\nRemoved";
        let new = "Keep the blue café!\n\nUnchanged\n";
        let (left, right) = changed_lines(old, new);
        let (a, b) = changed_words(old, new, &left, &right);
        assert_eq!(
            a[0].iter()
                .filter(|(_, changed)| *changed)
                .map(|(word, _)| *word)
                .collect::<Vec<_>>(),
            vec!["red", "."]
        );
        assert_eq!(
            b[0].iter()
                .filter(|(_, changed)| *changed)
                .map(|(word, _)| *word)
                .collect::<Vec<_>>(),
            vec!["blue", "!"]
        );
        assert!(a[1].is_empty() && b[1].is_empty() && b[3].is_empty());
        assert_eq!(a[2], vec![("Unchanged", false)]);
        assert_eq!(a[3], vec![("Removed", true)]);
        for (source, lines) in [(old, a), (new, b)] {
            assert_eq!(
                lines
                    .iter()
                    .map(|line| line.iter().map(|(word, _)| *word).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n"),
                source
            );
        }
    }
    #[test]
    fn diff_marks_insertions_without_marking_unchanged_lines() {
        assert_eq!(
            changed_lines("a\nb\nc", "a\nx\nb\nc"),
            (vec![false, false, false], vec![false, true, false, false])
        );
        assert_eq!(
            changed_lines("a\nb", "a\nc"),
            (vec![false, true], vec![false, true])
        );
    }
}
