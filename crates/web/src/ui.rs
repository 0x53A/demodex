//! Shared layout primitives for setup and administration.
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct SectionTitleProps { pub children: Children }
#[function_component(SectionTitle)]
pub fn section_title(props: &SectionTitleProps) -> Html {
    html!{<h3 class="section-accent">{props.children.clone()}</h3>}
}

#[derive(Properties, PartialEq)]
pub struct AddButtonProps {
    pub children: Children,
    pub onclick: Callback<MouseEvent>,
    #[prop_or_default] pub disabled: bool,
}
#[function_component(AddButton)]
pub fn add_button(props: &AddButtonProps) -> Html {
    html!{<button type="button" class="add-action" disabled={props.disabled} onclick={props.onclick.clone()}>{props.children.clone()}</button>}
}

#[derive(Properties, PartialEq)]
pub struct FieldActionProps { pub children: Children }
#[function_component(FieldAction)]
pub fn field_action(props: &FieldActionProps) -> Html {
    html!{<div class="field-action">{props.children.clone()}</div>}
}
