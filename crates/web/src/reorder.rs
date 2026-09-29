//! Pointer and keyboard ordering; a drop submits one guarded mutation.
use demodex_protocol::Operation;
use web_sys::{HtmlElement, PointerEvent};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub id: String,
    pub name: String,
    pub peers: Vec<(String, String)>,
    pub disabled: bool,
    pub onrun: Callback<Operation>,
}

struct Drag {
    pointer: i32,
    expected: Vec<String>,
    target: Option<(String, bool)>,
    x: i32,
    y: i32,
}

pub struct Handle { node: NodeRef, drag: Option<Drag> }
pub enum Msg { Start(PointerEvent), Move(PointerEvent), End(PointerEvent), Cancel, Key(KeyboardEvent) }

pub fn reordered(expected: &[String], id: &str, target: &str, after: bool) -> Vec<String> {
    if id == target || !expected.iter().any(|v| v == target) { return expected.to_vec(); }
    let mut ids: Vec<_> = expected.iter().filter(|v| *v != id).cloned().collect();
    let index = ids.iter().position(|v| v == target).unwrap() + usize::from(after);
    ids.insert(index, id.to_owned());
    ids
}

impl Component for Handle {
    type Message = Msg;
    type Properties = Props;
    fn create(_: &Context<Self>) -> Self { Self { node:NodeRef::default(), drag:None } }
    fn update(&mut self, ctx: &Context<Self>, message: Msg) -> bool {
        match message {
            Msg::Start(event) => {
                if ctx.props().disabled || !event.is_primary() || event.button() != 0 { return false; }
                event.prevent_default();
                if let Some(node) = self.node.cast::<HtmlElement>() {
                    let _ = node.focus();
                    if node.set_pointer_capture(event.pointer_id()).is_err() { return false; }
                }
                self.drag = Some(Drag { pointer:event.pointer_id(), expected:ctx.props().peers.iter().map(|p|p.0.clone()).collect(), target:None, x:event.client_x(), y:event.client_y() });
            }
            Msg::Move(event) => {
                let Some(drag) = &mut self.drag else { return false; };
                if event.pointer_id() != drag.pointer { return false; }
                event.prevent_default();
                drag.x = event.client_x(); drag.y = event.client_y();
                drag.target = web_sys::window().and_then(|w|w.document())
                    .and_then(|d| d.element_from_point(drag.x as f32, drag.y as f32))
                    .and_then(|e|e.closest(".tree-agent").ok().flatten())
                    .and_then(|e| {
                        let id = e.get_attribute("data-session-id")?;
                        if id == ctx.props().id || !drag.expected.contains(&id) { return None; }
                        let rect = e.get_bounding_client_rect();
                        Some((id, f64::from(drag.y) >= rect.top() + rect.height()/2.))
                    });
                if let Some(aside) = self.node.cast::<HtmlElement>().and_then(|e|e.closest("aside").ok().flatten()) {
                    let rect = aside.get_bounding_client_rect();
                    if f64::from(drag.y) < rect.top()+36. { aside.set_scroll_top(aside.scroll_top()-20); }
                    else if f64::from(drag.y) > rect.bottom()-36. { aside.set_scroll_top(aside.scroll_top()+20); }
                }
            }
            Msg::End(event) => {
                if self.drag.as_ref().is_none_or(|d|d.pointer != event.pointer_id()) { return false; }
                let drag = self.drag.take().unwrap();
                if let Some(node) = self.node.cast::<HtmlElement>() { let _ = node.release_pointer_capture(drag.pointer); }
                if !ctx.props().disabled && drag.expected.iter().eq(ctx.props().peers.iter().map(|p|&p.0)) {
                    if let Some((target, after)) = drag.target {
                        let ids = reordered(&drag.expected, &ctx.props().id, &target, after);
                        if ids != drag.expected { ctx.props().onrun.emit(Operation::ReorderSessions { expected:drag.expected, ids }); }
                    }
                }
            }
            Msg::Cancel => {
                if let Some(drag) = self.drag.take() {
                    if let Some(node) = self.node.cast::<HtmlElement>() { let _ = node.release_pointer_capture(drag.pointer); }
                }
            }
            Msg::Key(event) => {
                if event.key() == "Escape" { return yew::Component::update(self, ctx, Msg::Cancel); }
                if ctx.props().disabled || event.repeat() || self.drag.is_some() { return false; }
                let offset = match event.key().as_str() { "ArrowUp"=>-1, "ArrowDown"=>1, _=>return false };
                event.prevent_default();
                let peers = &ctx.props().peers;
                if let Some(index) = peers.iter().position(|p|p.0 == ctx.props().id).and_then(|i|i.checked_add_signed(offset)) {
                    if let Some((neighbor,_)) = peers.get(index) { ctx.props().onrun.emit(Operation::MoveSession{id:ctx.props().id.clone(),neighbor:neighbor.clone()}); }
                }
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let hint = self.drag.as_ref().map(|drag| match &drag.target {
            Some((id,after))=>format!("{} {}",if *after {"After"}else{"Before"},props.peers.iter().find(|p|&p.0==id).map(|p|p.1.as_str()).unwrap_or("session")),
            None=>"Drag within this group · Esc cancels".into(),
        });
        html!{<>
            <button type="button" ref={self.node.clone()} class="session-drag-handle" aria-label={format!("Reorder {}",props.name)} title="Drag to reorder · ↑ / ↓ to move" aria-pressed={self.drag.is_some().to_string()} disabled={props.disabled || props.peers.len()<2}
                onpointerdown={ctx.link().callback(Msg::Start)} onpointermove={ctx.link().callback(Msg::Move)} onpointerup={ctx.link().callback(Msg::End)} onpointercancel={ctx.link().callback(|_|Msg::Cancel)} onlostpointercapture={ctx.link().callback(|_|Msg::Cancel)} onkeydown={ctx.link().callback(Msg::Key)}>{"⠿"}</button>
            {if let Some(drag)=&self.drag {html!{<span class="reorder-hint" role="status" style={format!("left:{}px;top:{}px",drag.x.clamp(8,180),drag.y.saturating_sub(48).max(8))}>{hint.unwrap_or_default()}</span>}}else{Html::default()}}
        </>}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropping_moves_one_item_before_or_after_without_losing_peers() {
        let ids = vec!["a".into(),"b".into(),"c".into()];
        assert_eq!(reordered(&ids,"a","c",true),vec!["b","c","a"]);
        assert_eq!(reordered(&ids,"c","a",false),vec!["c","a","b"]);
        assert_eq!(reordered(&ids,"a","a",true),ids);
        assert_eq!(reordered(&ids,"a","foreign",true),ids);
    }
}
