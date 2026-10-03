use crate::{links::LinkContext, model::text};
use demodex_protocol::{LinkDestination, Operation};
use pulldown_cmark::{Event, Parser, Tag};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ImageProps {
    pub destination: String,
    #[prop_or_default]
    pub label: String,
}

/// Content paths never become browser resource URLs. Only authenticated,
/// signature-checked upload bytes or explicitly approved web URLs can render.
#[function_component(Image)]
pub fn image(props: &ImageProps) -> Html {
    let context = use_context::<LinkContext>().unwrap_or_default();
    let source = use_state(|| None::<(LinkContext, String, String)>);
    let error = use_state(String::new);
    let opened = use_state(|| false);
    let approved = use_state(|| None::<(LinkContext, String)>);
    let failed = use_state(|| false);
    let kind = demodex_protocol::links::classify(&props.destination);
    let local = matches!(&kind, LinkDestination::File { path } if path.starts_with('/'));
    {
        let source = source.clone();
        let error = error.clone();
        let opened = opened.clone();
        let approved = approved.clone();
        let failed = failed.clone();
        use_effect_with(
            (context.clone(), props.destination.clone()),
            move |(context, path)| {
                source.set(None);
                error.set(String::new());
                opened.set(false);
                approved.set(None);
                failed.set(false);
                let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
                if local {
                    if let Some(client) = context.client.clone().filter(|_| context.connected) {
                        let id = context.session.clone();
                        let identity = (context.clone(), path.clone());
                        let path = match demodex_protocol::links::classify(path) {
                            LinkDestination::File { path } => path,
                            _ => path.clone(),
                        };
                        let cancelled = cancelled.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            let result =
                                client.read(Operation::ReadUploadedImage { id, path }).await;
                            if cancelled.get() {
                                return;
                            }
                            match result {
                                Ok(value)
                                    if matches!(
                                        text(&value, "mime"),
                                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                                    ) =>
                                {
                                    source.set(Some((
                                        identity.0,
                                        identity.1,
                                        format!(
                                            "data:{};base64,{}",
                                            text(&value, "mime"),
                                            text(&value, "dataBase64")
                                        ),
                                    )));
                                }
                                Ok(_) => error.set("Unsupported image format".into()),
                                Err(e) => error.set(format!("{e:#}")),
                            }
                        });
                    } else {
                        error.set("Connect to preview this image".into());
                    }
                }
                move || cancelled.set(true)
            },
        );
    }
    let open = {
        let opened = opened.clone();
        Callback::from(move |_| opened.set(true))
    };
    let close = {
        let opened = opened.clone();
        let approved = approved.clone();
        let failed = failed.clone();
        Callback::from(move |_| {
            opened.set(false);
            approved.set(None);
            failed.set(false);
        })
    };
    let approve = {
        let approved = approved.clone();
        let identity = (context.clone(), props.destination.clone());
        Callback::from(move |_| approved.set(Some(identity.clone())))
    };
    let onerror = {
        let failed = failed.clone();
        Callback::from(move |_| failed.set(true))
    };
    let source = source
        .as_ref()
        .filter(|(c, p, _)| c == &context && p == &props.destination)
        .map(|(_, _, src)| src);
    let consent = approved
        .as_ref()
        .is_some_and(|(c, p)| c == &context && p == &props.destination);
    let label = if props.label.is_empty() {
        "Image"
    } else {
        &props.label
    };
    html! {<span class="image-widget">
        <button type="button" class={classes!("image-button",(!local).then_some("remote-image-button"))} title={props.destination.clone()} aria-label={format!("Preview {label}")} onclick={open}>
            {if let Some(src)=source { html!{<img class="image-thumbnail" src={src.clone()} alt={label.to_owned()}/>} }
            else if local { html!{<span>{if error.is_empty(){"◻"}else{"▧"}}{" "}{label}</span>} }
            else { html!{<><span class="image-warning" aria-label="External image">{"⚠"}</span>{" "}{label}</>} }}
        </button>
        {if *opened {html!{<crate::modal::Modal title="Image preview" icon_close=true dismiss_outside=true onclose={close}>
            <div class="link-destination">{&props.destination}</div>
            {if local {
                if let Some(src)=source {html!{<img class="image-expanded" src={src.clone()} alt={label.to_owned()}/>}}
                else {html!{<p role="status">{if error.is_empty(){"Loading image…"}else{&error}}</p>}}
            } else { match &kind {
                LinkDestination::Web {url,host,warnings} => {
                    let start=url.find("://").unwrap()+3;
                    html!{<><p class="link-destination">{&url[..start]}<strong>{host}</strong>{&url[start+host.len()..]}</p>
                        {for warnings.iter().map(|warning|html!{<p class="link-warning">{warning}</p>})}
                        {if consent {
                            html!{<><img class="image-expanded" src={url.clone()} alt={label.to_owned()} referrerpolicy="no-referrer" {onerror}/>{if *failed{html!{<p role="alert">{"Image could not be loaded."}</p>}}else{Html::default()}}</>}
                        }else{html!{<><p class="link-warning">{"Loading contacts this website and reveals your IP address. The request may send cookies and follow redirects."}</p><button type="button" onclick={approve}>{"Load image"}</button></>}}}
                    </>}
                }
                _ => html!{<p>{"Only HTTP(S) images and recorded local uploads can be previewed."}</p>},
            }}}
        </crate::modal::Modal>}}else{Html::default()}}
    </span>}
}

pub fn destinations(source: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for event in Parser::new(source) {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            let path = dest_url.to_string();
            if !paths.contains(&path) {
                paths.push(path);
            }
            if paths.len() == 10 {
                break;
            }
        }
    }
    paths
}

#[derive(Properties, PartialEq)]
pub struct DraftProps {
    pub source: String,
}

#[function_component(DraftImages)]
pub fn draft_images(props: &DraftProps) -> Html {
    let paths = destinations(&props.source);
    if paths.is_empty() {
        return Html::default();
    }
    html! {<div class="composer-images" aria-label="Attached image previews">{for paths.into_iter().map(|path|html!{<Image key={path.clone()} destination={path.clone()} label="Attached image"/>})}</div>}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draft_images_ignore_code_and_deduplicate() {
        assert_eq!(
            destinations(
                "`![x](/ignored)` ![a](</a b.png>) ![again](</a b.png>) ![web](https://example.org/a.png)"
            ),
            vec!["/a b.png", "https://example.org/a.png"]
        );
        assert!(destinations("```\n![x](/ignored)\n```").is_empty());
    }
}
