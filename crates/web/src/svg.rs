//! Bounded static SVG images. Validated XML is loaded through an img data URL,
//! never inserted into the application DOM. Local paint servers are supported;
//! external references, scripts, CSS, HTML and animation remain unsupported.
use crate::modal::Modal;
use base64::Engine;
use std::collections::HashMap;
use yew::prelude::*;

const MAX_BYTES: usize = 128 * 1024;
const MAX_NODES: usize = 2048;
const MAX_DEPTH: usize = 32;
const MAX_ATTRIBUTE: usize = 16 * 1024;
const SVG_NS: &str = "http://www.w3.org/2000/svg";

type SvgResult<T> = Result<T, String>;

fn numbers(value: &str) -> Option<Vec<f64>> {
    let result: Vec<_> = value
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(str::parse::<f64>)
        .collect::<Result<_, _>>()
        .ok()?;
    (!result.is_empty()
        && result
            .iter()
            .all(|n| n.is_finite() && n.abs() <= 1_000_000.0))
    .then_some(result)
}
fn number(value: &str) -> Option<f64> {
    let n = numbers(value)?;
    (n.len() == 1).then_some(n[0])
}
fn color(value: &str) -> bool {
    if let Some(hex) = value.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    matches!(
        value,
        "none"
            | "currentColor"
            | "transparent"
            | "black"
            | "white"
            | "gray"
            | "grey"
            | "red"
            | "green"
            | "blue"
            | "yellow"
            | "orange"
            | "purple"
            | "pink"
            | "cyan"
            | "magenta"
            | "teal"
            | "navy"
            | "lime"
            | "silver"
            | "maroon"
            | "olive"
            | "aqua"
            | "fuchsia"
    )
}
fn transform(mut value: &str) -> bool {
    let mut count = 0;
    while !value.trim().is_empty() {
        value = value.trim_start();
        let Some((name, rest)) = value.split_once('(') else {
            return false;
        };
        let Some((args, rest)) = rest.split_once(')') else {
            return false;
        };
        let Some(args) = numbers(args) else {
            return false;
        };
        if !match name.trim() {
            "matrix" => args.len() == 6,
            "translate" | "scale" => matches!(args.len(), 1 | 2),
            "rotate" => matches!(args.len(), 1 | 3),
            "skewX" | "skewY" => args.len() == 1,
            _ => false,
        } {
            return false;
        }
        count += 1;
        if count > 16 {
            return false;
        }
        value = rest.trim_start().strip_prefix(',').unwrap_or(rest);
    }
    count > 0
}
fn path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() || c == b',' || b"MmZzLlHhVvCcSsQqTtAa".contains(&c) {
            i += 1;
            continue;
        }
        let start = i;
        if matches!(c, b'+' | b'-') {
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
            i += 1;
            if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i == start || number(&value[start..i]).is_none() {
            return false;
        }
    }
    !value.is_empty()
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn paint_reference(value: &str) -> Option<&str> {
    value
        .strip_prefix("url(#")
        .and_then(|s| s.strip_suffix(')'))
        .filter(|s| identifier(s))
}
fn coordinate(value: &str) -> bool {
    number(value.strip_suffix('%').unwrap_or(value)).is_some()
}
fn attribute(name: &str, value: &str) -> bool {
    if value.len() > MAX_ATTRIBUTE {
        return false;
    }
    match name {
        "x" | "y" | "x1" | "y1" | "x2" | "y2" | "cx" | "cy" | "dx" | "dy" => {
            number(value).is_some()
        }
        "width" | "height" | "r" | "rx" | "ry" | "stroke-width" | "stroke-miterlimit"
        | "font-size" | "textLength" => number(value).is_some_and(|n| n >= 0.0),
        "opacity" | "fill-opacity" | "stroke-opacity" => {
            number(value).is_some_and(|n| (0.0..=1.0).contains(&n))
        }
        "fill" | "stroke" => color(value) || paint_reference(value).is_some(),
        "stop-color" => color(value),
        "stop-opacity" => number(value).is_some_and(|n| (0.0..=1.0).contains(&n)),
        "offset" => match value.strip_suffix('%') {
            Some(v) => number(v).is_some_and(|n| (0.0..=100.0).contains(&n)),
            None => number(value).is_some_and(|n| (0.0..=1.0).contains(&n)),
        },
        "id" => identifier(value),
        "role" => value == "img",
        "aria-label" => !value.is_empty(),
        "aria-labelledby" | "aria-describedby" => {
            !value.trim().is_empty()
                && value.split_ascii_whitespace().count() <= 16
                && value.split_ascii_whitespace().all(identifier)
        }
        "letter-spacing" | "word-spacing" => number(value).is_some(),
        "gradientUnits" | "patternUnits" | "patternContentUnits" => {
            matches!(value, "userSpaceOnUse" | "objectBoundingBox")
        }
        "gradientTransform" | "patternTransform" => transform(value),
        "spreadMethod" => matches!(value, "pad" | "reflect" | "repeat"),
        "transform" => transform(value),
        "points" => numbers(value).is_some_and(|n| n.len() >= 4 && n.len() % 2 == 0),
        // Only geometry command letters and bounded finite numbers enter the image.
        "d" => path(value),
        "stroke-linecap" => matches!(value, "butt" | "round" | "square"),
        "stroke-linejoin" => matches!(value, "miter" | "round" | "bevel"),
        "fill-rule" | "clip-rule" => matches!(value, "nonzero" | "evenodd"),
        "stroke-dasharray" => {
            value == "none" || numbers(value).is_some_and(|n| n.iter().all(|v| *v >= 0.0))
        }
        "stroke-dashoffset" => number(value).is_some(),
        "text-anchor" => matches!(value, "start" | "middle" | "end"),
        "dominant-baseline" => matches!(
            value,
            "auto"
                | "middle"
                | "central"
                | "hanging"
                | "alphabetic"
                | "text-before-edge"
                | "text-after-edge"
        ),
        "font-family" => matches!(value, "sans-serif" | "serif" | "monospace"),
        "font-weight" => matches!(
            value,
            "normal"
                | "bold"
                | "100"
                | "200"
                | "300"
                | "400"
                | "500"
                | "600"
                | "700"
                | "800"
                | "900"
        ),
        "font-style" => matches!(value, "normal" | "italic" | "oblique"),
        "lengthAdjust" => matches!(value, "spacing" | "spacingAndGlyphs"),
        "preserveAspectRatio" => {
            let words: Vec<_> = value.split_ascii_whitespace().collect();
            matches!(words.as_slice(), ["none"])
                || words.len() <= 2
                    && words.first().is_some_and(|v| {
                        matches!(
                            *v,
                            "xMinYMin"
                                | "xMidYMin"
                                | "xMaxYMin"
                                | "xMinYMid"
                                | "xMidYMid"
                                | "xMaxYMid"
                                | "xMinYMax"
                                | "xMidYMax"
                                | "xMaxYMax"
                        )
                    })
                    && words.get(1).is_none_or(|v| matches!(*v, "meet" | "slice"))
        }
        "viewBox" => numbers(value).is_some_and(|n| n.len() == 4 && n[2] > 0.0 && n[3] > 0.0),
        _ => false,
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// Re-serialize the validated subset, supplying an SVG namespace even for fenced
// snippets that omit it. The original source remains untouched in source controls.
fn image_source(source: &str) -> SvgResult<String> {
    if source.len() > MAX_BYTES {
        return Err("SVG exceeds the 128 KiB preview limit".into());
    }
    let doc = roxmltree::Document::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES as u32,
            ..Default::default()
        },
    )
    .map_err(|_| "SVG is invalid or exceeds the 2,048-node limit".to_owned())?;
    if doc.descendants().any(|n| n.is_pi()) {
        return Err("SVG processing instructions are unsupported".into());
    }
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return Err("Expected an SVG root element".into());
    }
    let view_box = match root.attribute("viewBox") {
        Some(v) if attribute("viewBox", v) => v.to_owned(),
        Some(_) => return Err("Invalid SVG viewBox".into()),
        None => {
            let dimensions = root
                .attribute("width")
                .and_then(number)
                .zip(root.attribute("height").and_then(number));
            let Some((w, h)) = dimensions.filter(|(w, h)| *w > 0.0 && *h > 0.0) else {
                return Err("SVG needs a viewBox or positive numeric width and height".into());
            };
            format!("0 0 {w} {h}")
        }
    };
    let mut ids = HashMap::new();
    let mut paint_servers = 0;
    for n in root.descendants().filter(|n| n.is_element()) {
        if let Some(id) = n.attribute("id") {
            if !identifier(id) || ids.insert(id, n.tag_name().name()).is_some() {
                return Err("Invalid or duplicate SVG ID".into());
            }
        }
        if matches!(
            n.tag_name().name(),
            "linearGradient" | "radialGradient" | "pattern"
        ) {
            paint_servers += 1;
            if paint_servers > 32 {
                return Err("SVG exceeds the 32 paint-server limit".into());
            }
        }
    }
    fn node(
        n: roxmltree::Node<'_, '_>,
        depth: usize,
        ids: &HashMap<&str, &str>,
        view_box: &str,
        output: &mut String,
    ) -> SvgResult<()> {
        if depth > MAX_DEPTH {
            return Err("SVG exceeds the 32-level nesting limit".into());
        }
        if n.is_text() {
            output.push_str(&escape(n.text().unwrap_or_default()));
            return Ok(());
        }
        if n.is_comment() {
            return Ok(());
        }
        if !n.is_element() {
            return Err("Unsupported SVG content".into());
        }
        let name = n.tag_name().name();
        let parent = n.parent_element().map(|p| p.tag_name().name());
        if n.tag_name().namespace().is_some_and(|ns| ns != SVG_NS)
            || !matches!(
                name,
                "svg"
                    | "g"
                    | "rect"
                    | "circle"
                    | "ellipse"
                    | "line"
                    | "polyline"
                    | "polygon"
                    | "path"
                    | "text"
                    | "tspan"
                    | "title"
                    | "desc"
                    | "defs"
                    | "linearGradient"
                    | "radialGradient"
                    | "stop"
                    | "pattern"
            )
            || name == "svg" && depth != 0
        {
            return Err(format!("Unsupported SVG element: {name}"));
        }
        let gradient = matches!(name, "linearGradient" | "radialGradient");
        if (name == "defs" && parent != Some("svg"))
            || ((gradient || name == "pattern") && parent != Some("defs"))
            || (name == "stop" && !matches!(parent, Some("linearGradient" | "radialGradient")))
            || (matches!(parent, Some("linearGradient" | "radialGradient")) && name != "stop")
        {
            return Err(format!("Unsupported SVG placement: {name}"));
        }
        if name == "pattern" {
            // Limit tile density in either coordinate system. Nested/reference-
            // based patterns are excluded below rather than expanding a graph.
            let minimum = if n.attribute("patternUnits") == Some("userSpaceOnUse") {
                1.0
            } else {
                0.01
            };
            for key in ["width", "height"] {
                if !n
                    .attribute(key)
                    .and_then(number)
                    .is_some_and(|v| v >= minimum)
                {
                    return Err(
                        "SVG pattern needs bounded positive numeric width and height".into(),
                    );
                }
            }
        }
        output.push('<');
        output.push_str(name);
        if depth == 0 {
            output.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
        }
        for a in n.attributes() {
            let key = a.name();
            let value = a.value();
            let gradient_coordinate = gradient
                && matches!(
                    key,
                    "x1" | "y1" | "x2" | "y2" | "cx" | "cy" | "r" | "fx" | "fy" | "fr"
                );
            if a.namespace().is_some()
                || value.len() > MAX_ATTRIBUTE
                || !(if gradient_coordinate {
                    coordinate(value)
                } else {
                    attribute(key, value)
                })
            {
                return Err(format!("Unsupported SVG attribute: {key}"));
            }
            if let Some(id) = paint_reference(value).filter(|_| matches!(key, "fill" | "stroke")) {
                if !matches!(
                    ids.get(id),
                    Some(&"linearGradient" | &"radialGradient" | &"pattern")
                ) {
                    return Err("SVG paint reference must name a local paint server".into());
                }
                if n.ancestors().any(|p| p.has_tag_name("pattern"))
                    || n.descendants().any(|p| p.has_tag_name("defs"))
                    || matches!(name, "defs" | "linearGradient" | "radialGradient" | "stop")
                {
                    return Err("SVG paint references inside definitions are unsupported".into());
                }
            }
            if matches!(key, "aria-labelledby" | "aria-describedby")
                && !value
                    .split_ascii_whitespace()
                    .all(|id| matches!(ids.get(id), Some(&"title" | &"desc")))
            {
                return Err(
                    "SVG accessibility reference must name a local title or description".into(),
                );
            }
            if depth == 0 && matches!(key, "width" | "height") {
                continue;
            }
            output.push(' ');
            output.push_str(key);
            output.push_str("=\"");
            output.push_str(&escape(value));
            output.push('"');
        }
        if depth == 0 && n.attribute("viewBox").is_none() {
            output.push_str(" viewBox=\"");
            output.push_str(&escape(view_box));
            output.push('"');
        }
        output.push('>');
        for child in n.children() {
            node(child, depth + 1, ids, view_box, output)?;
        }
        output.push_str("</");
        output.push_str(name);
        output.push('>');
        Ok(())
    }
    let mut normalized = String::new();
    node(root, 0, &ids, &view_box, &mut normalized)?;
    Ok(normalized)
}

pub fn render(source: &str) -> SvgResult<Html> {
    let source = image_source(source)?;
    let doc = roxmltree::Document::parse(&source).map_err(|_| "Invalid normalized SVG")?;
    let root = doc.root_element();
    let referenced_label = root.attribute("aria-labelledby").map(|ids| {
        ids.split_ascii_whitespace()
            .filter_map(|id| root.descendants().find(|n| n.attribute("id") == Some(id)))
            .flat_map(|n| {
                n.descendants()
                    .filter(|n| n.is_text())
                    .filter_map(|n| n.text())
            })
            .flat_map(|text| text.chars().chain(std::iter::once(' ')))
            .take(4096)
            .collect::<String>()
            .trim_end()
            .to_owned()
    });
    let title = root.children().find(|n| n.has_tag_name("title")).map(|n| {
        n.descendants()
            .filter(|n| n.is_text())
            .filter_map(|n| n.text())
            .collect::<String>()
    });
    let label = referenced_label
        .as_deref()
        .or_else(|| root.attribute("aria-label"))
        .or(title.as_deref())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("SVG preview")
        .chars()
        .take(4096)
        .collect::<String>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    let uri = format!("data:image/svg+xml;base64,{encoded}");
    Ok(html! { <SvgImage key={uri.clone()} source={uri.clone()} {label}/> })
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub source: AttrValue,
}

// Image documents isolate IDs and prohibit scripts/external loads in the browser.
// Decode failure still leaves the exact source and file download available.
#[derive(Properties, PartialEq)]
struct ImageProps {
    source: AttrValue,
    label: AttrValue,
}

#[function_component(SvgImage)]
fn svg_image(props: &ImageProps) -> Html {
    let failed = use_state(|| false);
    let onerror = {
        let failed = failed.clone();
        Callback::from(move |_| failed.set(true))
    };
    html! { <>
        <img src={props.source.clone()} alt={props.label.clone()} {onerror}/>
        {if *failed { html! { <p role="alert">{"SVG preview unavailable: browser could not decode the image."}</p> } } else { Html::default() }}
    </> }
}

#[function_component(SvgPreview)]
pub fn svg_preview(props: &Props) -> Html {
    let expanded = use_state(|| false);
    let parsed = use_memo(props.source.clone(), |source| render(source));
    let show = {
        let expanded = expanded.clone();
        Callback::from(move |_| expanded.set(true))
    };
    let close = {
        let expanded = expanded.clone();
        Callback::from(move |_| expanded.set(false))
    };
    html! {
        <div class="svg-widget">
            {match &*parsed {
                Ok(tree) => html! {
                    <>
                        <button type="button" class="svg-thumbnail" onclick={show} aria-label="Enlarge SVG preview" title="Enlarge SVG preview">{tree.clone()}</button>
                        {if *expanded { html! {
                            <Modal title="SVG preview" onclose={close} icon_close=true dismiss_outside=true>
                                <div class="svg-expanded">{tree.clone()}</div>
                            </Modal>
                        }} else { Html::default() }}
                    </>
                },
                Err(error) => html! { <p class="muted">{format!("SVG preview unavailable: {error}.")}</p> },
            }}
            <details class="svg-source"><summary>{"SVG source"}</summary><pre><code>{props.source.clone()}</code></pre></details>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_local_geometry_and_text() {
        assert!(render(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><title>Example</title><g transform="translate(10, 5) rotate(3)"><rect width="80" height="40" fill="#ff8800"/><circle cx="120" cy="30" r="20" stroke="white"/><path d="M 0 0 L 10 20 Z"/><text x="20" y="80" font-family="sans-serif">Hello &amp; welcome</text></g></svg>"##).is_ok());
        assert!(render(r#"<svg width="100" height="50"><line x1="0" x2="50" /></svg>"#).is_ok());
    }
    #[test]
    fn supports_rover_and_normalizes_namespace_without_inserting_markup() {
        let rover = include_str!("../../../tests/fixtures/mars-rover.svg");
        let output = image_source(rover).unwrap();
        assert!(output.contains("url(#solar)"));
        assert!(output.contains("aria-labelledby=\"title desc\""));
        assert!(render(rover).is_ok());
        let minimal = image_source(
            r#"<svg width="100" height="50"><text>&lt;script&gt; &amp; &quot;</text></svg>"#,
        )
        .unwrap();
        let doc = roxmltree::Document::parse(&minimal).unwrap();
        assert_eq!(doc.root_element().tag_name().namespace(), Some(SVG_NS));
        assert_eq!(doc.root_element().attribute("viewBox"), Some("0 0 100 50"));
        assert_eq!(
            doc.root_element().first_element_child().unwrap().text(),
            Some("<script> & \"")
        );
        assert!(image_source(r#"<svg viewBox="0 0 10 10"><defs><radialGradient id="g" cx="50%" r="50%"><stop offset="100%" stop-color="red"/></radialGradient></defs><circle r="5" fill="url(#g)"/></svg>"#).is_ok());
    }
    #[test]
    fn rejects_active_content_external_or_recursive_references() {
        for body in [
            r#"<script>alert(1)</script>"#,
            r#"<foreignObject/>"#,
            r#"<image href="https://example.com/pixel"/>"#,
            r##"<use href="#shape"/>"##,
            r#"<rect onload="alert(1)"/>"#,
            r#"<rect fill="url(https://example.com/paint.svg#g)"/>"#,
            r#"<rect fill="url(#missing)"/>"#,
            r#"<rect id="g"/><circle fill="url(#g)"/>"#,
            r#"<title id="a"/><desc id="a"/>"#,
            r#"<text aria-labelledby="missing"/>"#,
            r##"<defs><linearGradient id="g" href="#g"/></defs>"##,
            r#"<defs><pattern id="p" width="1" height="1"><rect fill="url(#p)"/></pattern></defs>"#,
            r#"<defs><pattern width="0.00001" height="1"/></defs>"#,
            r#"<defs><pattern width="1" height="1"><defs/></pattern></defs>"#,
        ] {
            assert!(
                image_source(&format!("<svg viewBox=\"0 0 10 10\">{body}</svg>")).is_err(),
                "{body}"
            );
        }
        assert!(
            image_source(r#"<!DOCTYPE svg [<!ENTITY x "bad">]><svg viewBox="0 0 10 10">&x;</svg>"#)
                .is_err()
        );
        assert!(
            image_source(
                r#"<?xml-stylesheet href="https://example.com/x"?><svg viewBox="0 0 10 10"/>"#
            )
            .is_err()
        );
        assert!(
            image_source(&format!(
                "<svg viewBox=\"0 0 10 10\"><defs>{}</defs></svg>",
                "<linearGradient/>".repeat(33)
            ))
            .is_err()
        );
    }
    #[test]
    fn unsupported_features_retain_fallback() {
        for source in [
            r#"<svg viewBox="0 0 10 10"><filter/></svg>"#,
            r#"<svg viewBox="0 0 10 10" style="fill:red"/>"#,
            r#"<svg viewBox="0 0 10 10"><image/></svg>"#,
            r#"<svg viewBox="0 0 10 10"><rect width="100%"/></svg>"#,
        ] {
            assert!(render(source).is_err());
        }
    }
    #[test]
    fn enforces_size_depth_nodes_and_finite_dimensions() {
        assert!(render(&" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(
            render(&format!(
                "<svg viewBox=\"0 0 10 10\">{}{}</svg>",
                "<g>".repeat(34),
                "</g>".repeat(34)
            ))
            .is_err()
        );
        assert!(
            render(&format!(
                "<svg viewBox=\"0 0 10 10\">{}</svg>",
                "<g/>".repeat(MAX_NODES)
            ))
            .is_err()
        );
        for value in ["0 0 0 10", "0 0 NaN 10", "0 0 1e30 10"] {
            assert!(!attribute("viewBox", value));
        }
        assert!(path("M0-1.5L2e2,3z"));
        assert!(!path("M 0 0 L 1e30 10"));
        assert!(!path("M 0 0 L NaN 10"));
        assert!(!transform(&"scale(1) ".repeat(17)));
        assert!(!transform("scale(NaN)"));
        assert!(!transform("translate(1,2,3)"));
        assert!(!attribute("aria-labelledby", &"label ".repeat(17)));
    }
}
