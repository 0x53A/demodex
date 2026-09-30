//! On-demand score playback. File acquisition stays with the host.
use wasm_bindgen::prelude::*;
use yew::prelude::*;

#[wasm_bindgen(module = "/js/music-player.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = mountScore)]
    async fn mount_score(
        host: &web_sys::Element,
        source: &str,
        filename: &str,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = unmountScore)]
    fn unmount_score(host: &web_sys::Element);
}

#[derive(Properties, PartialEq)]
pub struct ScoreProps {
    pub source: String,
    #[prop_or_else(|| "composition.eod".into())]
    pub filename: String,
}

#[function_component(ScorePlayer)]
pub fn score_player(props: &ScoreProps) -> Html {
    let host = use_node_ref();
    let error = use_state(String::new);
    let loaded = use_state(|| false);
    {
        let host = host.clone();
        let error = error.clone();
        let loaded = loaded.clone();
        use_effect_with(
            (props.source.clone(), props.filename.clone()),
            move |(source, filename)| {
                let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
                let finished = cancelled.clone();
                let source = source.clone();
                let filename = filename.clone();
                let element = host.cast::<web_sys::Element>().unwrap();
                let mounting = element.clone();
                error.set(String::new());
                loaded.set(false);
                wasm_bindgen_futures::spawn_local(async move {
                    let result = mount_score(&mounting, &source, &filename).await;
                    if !finished.get() {
                        match result {
                            Ok(_) => loaded.set(true),
                            Err(value) => {
                                let message = js_sys::Reflect::get(&value, &"message".into())
                                    .ok()
                                    .and_then(|value| value.as_string())
                                    .or_else(|| value.as_string())
                                    .unwrap_or_else(|| "Cannot open the music player.".into());
                                error.set(message);
                            }
                        }
                    }
                });
                move || {
                    cancelled.set(true);
                    unmount_score(&element);
                }
            },
        );
    }
    html! {<section class="score-player">
        {if !error.is_empty() { html!{<p class="error" role="alert">{(*error).clone()}</p>} }
         else if !*loaded { html!{<p role="status">{"Loading Apteronotus…"}</p>} }
         else { Html::default() }}
        <div class="score-player-host" ref={host}/>
    </section>}
}

/// Only explicitly tagged, complete score fences offer playback. Keep source
/// bytes (including CRLF) intact; ordinary Lua snippets are not scores.
pub fn score_sources(text: &str) -> Vec<&str> {
    let mut scores = Vec::new();
    let mut fence: Option<(char, usize, Option<usize>)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let content = trimmed.trim_start_matches(' ');
        if trimmed.len() - content.len() <= 3 {
            let marker = content.chars().next().unwrap_or(' ');
            let count = content.chars().take_while(|&c| c == marker).count();
            if let Some((opening, width, start)) = fence {
                if marker == opening && count >= width && content[count..].trim().is_empty() {
                    if let Some(start) = start {
                        scores.push(&text[start..offset]);
                    }
                    fence = None;
                }
            } else if matches!(marker, '`' | '~') && count >= 3 {
                let language = content[count..].trim();
                let score = matches!(language, "eod" | "apteronotus" | "apt");
                fence = Some((marker, count, score.then_some(offset + line.len())));
            }
        }
        offset += line.len();
    }
    scores
}

#[derive(Properties, PartialEq)]
pub struct ScoresProps {
    pub text: String,
}

#[function_component(MessageScores)]
pub fn message_scores(props: &ScoresProps) -> Html {
    // Opening selects an immutable snapshot. Later transcript edits cannot
    // replace a score or tear down audio without another explicit open.
    let selected = use_state(|| None::<String>);
    let sources = score_sources(&props.text);
    html! {<>
        {for sources.iter().enumerate().map(|(index, source)| {
            let selected = selected.clone();
            let source = (*source).to_owned();
            html!{<button class="open-score" onclick={Callback::from(move |_| selected.set(Some(source.clone())))}>
                {format!("Open Apteronotus score {}", index + 1)}
            </button>}
        })}
        {if let Some(source) = selected.as_ref() {
            let selected = selected.clone();
            html!{<><button onclick={Callback::from(move |_| selected.set(None))}>{"Close player"}</button>
                <ScorePlayer source={source.clone()}/></>}
        } else {Html::default()}}
    </>}
}

#[cfg(test)]
mod tests {
    use super::score_sources;

    #[test]
    fn scores_require_complete_explicit_fences_and_keep_exact_source() {
        assert_eq!(
            score_sources("```eod\r\n-- 水\r\ntempo(90)\r\n```\r\n"),
            vec!["-- 水\r\ntempo(90)\r\n"]
        );
        assert!(score_sources("```lua\nvoice {}\n```\n").is_empty());
        assert!(score_sources("```eod\nunfinished").is_empty());
        assert!(score_sources("````text\n```eod\nnested\n```\n````\n").is_empty());
        assert_eq!(
            score_sources("~~~apteronotus\na\n~~~\n```apt\nb\n```"),
            vec!["a\n", "b\n"]
        );
    }
}
