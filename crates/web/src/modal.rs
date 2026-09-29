use wasm_bindgen::JsCast;
use web_sys::{HtmlDialogElement, HtmlElement, KeyboardEvent};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub title: String,
    #[prop_or_default]
    pub compact: bool,
    #[prop_or_default]
    pub dismiss_outside: bool,
    #[prop_or_default]
    pub icon_close: bool,
    pub onclose: Callback<()>,
    pub children: Children,
}

pub struct Modal {
    dialog: NodeRef,
}
impl Component for Modal {
    type Message = ();
    type Properties = Props;
    fn create(_: &Context<Self>) -> Self {
        Self {
            dialog: NodeRef::default(),
        }
    }
    fn rendered(&mut self, _: &Context<Self>, first: bool) {
        if first && let Some(dialog) = self.dialog.cast::<HtmlDialogElement>() {
            // Native modal semantics provide focus containment and an inert background.
            let _ = dialog.show_modal();
        }
    }
    fn update(&mut self, ctx: &Context<Self>, _: ()) -> bool {
        if let Some(dialog) = self.dialog.cast::<HtmlDialogElement>() {
            dialog.close();
        }
        ctx.props().onclose.emit(());
        false
    }
    fn destroy(&mut self, _: &Context<Self>) {
        if let Some(dialog) = self.dialog.cast::<HtmlDialogElement>() {
            dialog.close();
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let reference = self.dialog.clone();
        let trap_focus = Callback::from(move |event: KeyboardEvent| {
            if event.key() != "Tab" {
                return;
            }
            let Some(dialog) = reference.cast::<HtmlDialogElement>() else {
                return;
            };
            let Ok(nodes) = dialog
                .query_selector_all("button,input,select,textarea,a[href],summary,[tabindex]")
            else {
                return;
            };
            let focusable: Vec<HtmlElement> = (0..nodes.length())
                .filter_map(|index| nodes.item(index)?.dyn_into::<HtmlElement>().ok())
                .filter(|element| {
                    !element.matches(":disabled").unwrap_or(true)
                        && element.tab_index() >= 0
                        && (element.offset_width() > 0 || element.offset_height() > 0)
                })
                .collect();
            let Some(active) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.active_element())
            else {
                return;
            };
            if let (Some(first), Some(last)) = (focusable.first(), focusable.last()) {
                let target = if event.shift_key()
                    && first.unchecked_ref::<web_sys::Element>() == &active
                {
                    Some(last)
                } else if !event.shift_key() && last.unchecked_ref::<web_sys::Element>() == &active
                {
                    Some(first)
                } else {
                    None
                };
                if let Some(target) = target {
                    event.prevent_default();
                    let _ = target.focus();
                }
            }
        });
        let reference = self.dialog.clone();
        let close = ctx.props().onclose.clone();
        let dismiss = ctx.props().dismiss_outside;
        let outside = Callback::from(move |event: MouseEvent| {
            if !dismiss { return; }
            if let Some(dialog) = reference.cast::<HtmlDialogElement>() {
                // Nested preview dialogs bubble clicks through this dialog too.
                // Only this dialog's own backdrop is an outside-click target.
                if event.target().is_none_or(|target| target != dialog.clone().unchecked_into::<web_sys::EventTarget>()) { return; }
                let r = dialog.get_bounding_client_rect();
                let (x,y) = (event.client_x() as f64,event.client_y() as f64);
                if x < r.left() || x > r.right() || y < r.top() || y > r.bottom() { close.emit(()); }
            }
        });
        html! {<dialog class={classes!("session-modal", ctx.props().compact.then_some("content-sized"))} ref={self.dialog.clone()} aria-label={ctx.props().title.clone()} onkeydown={trap_focus}
            onclick={outside} oncancel={ctx.link().callback(|e:Event|{e.prevent_default();})}>
            <div class="modal-heading"><h2>{&ctx.props().title}</h2><button type="button" aria-label="Close" autofocus=true onclick={ctx.link().callback(|_|())}>{if ctx.props().icon_close {"×"} else {"Close"}}</button></div>
            <div class="modal-body">{ctx.props().children.clone()}</div>
        </dialog>}
    }
}
