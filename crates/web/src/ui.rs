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

/// Shared frame for related settings and setup fields.
#[derive(Properties, PartialEq)]
pub struct GroupProps {
    #[prop_or_default] pub title: AttrValue,
    #[prop_or_default] pub label: Option<AttrValue>,
    #[prop_or_default] pub class: Classes,
    pub children: Children,
}
#[function_component(Group)]
pub fn group(props: &GroupProps) -> Html {
    let label = props.label.clone().or_else(|| (!props.title.is_empty()).then(|| props.title.clone()));
    html! {<section class={classes!("ui-group",props.class.clone())} aria-label={label}>
        {if props.title.is_empty(){Html::default()}else{html!{<SectionTitle>{props.title.clone()}</SectionTitle>}}}
        {props.children.clone()}
    </section>}
}

#[derive(Properties, PartialEq)]
pub struct IconButtonProps {
    pub label: AttrValue,
    #[prop_or_default] pub title: Option<AttrValue>,
    #[prop_or_default] pub class: Classes,
    #[prop_or_default] pub disabled: bool,
    #[prop_or_default] pub destructive: bool,
    #[prop_or_default] pub pressed: Option<AttrValue>,
    #[prop_or_default] pub autofocus: bool,
    pub onclick: Callback<MouseEvent>,
    pub children: Html,
}
#[function_component(IconButton)]
pub fn icon_button(props: &IconButtonProps) -> Html {
    html!{<button type="button" class={classes!("icon-button",props.class.clone(),props.destructive.then_some("destructive"))}
        aria-label={props.label.clone()} title={props.title.clone().unwrap_or_else(||props.label.clone())}
        aria-pressed={props.pressed.clone()} disabled={props.disabled} autofocus={props.autofocus} onclick={props.onclick.clone()}>
        <span aria-hidden="true">{props.children.clone()}</span>
    </button>}
}

#[derive(Properties, PartialEq)]
pub struct ActionsProps { pub children: Children }
#[function_component(Actions)]
pub fn actions(props: &ActionsProps) -> Html {
    html!{<div class="form-actions">{props.children.clone()}</div>}
}

#[derive(Properties, PartialEq)]
pub struct FormProps {
    #[prop_or_default] pub class: Classes,
    pub onsubmit: Callback<SubmitEvent>,
    #[prop_or_default] pub actions: Html,
    pub children: Children,
}
#[function_component(Form)]
pub fn form(props: &FormProps) -> Html {
    use wasm_bindgen::JsCast;
    let node=use_node_ref();
    let onsubmit=Callback::from({let node=node.clone();let submit=props.onsubmit.clone();move |event:SubmitEvent|{
        event.prevent_default();
        // Nested dialogs can live inside a form's DOM subtree. A submit belongs
        // only to its own form; it must never submit the dialog underneath it.
        event.stop_propagation();
        let Some(form)=node.cast::<web_sys::Element>() else{return};
        if event.target().as_ref()!=Some(form.unchecked_ref()) {return;}
        let Ok(fields)=form.query_selector_all("input,select,textarea") else{return};
        let mut first:Option<web_sys::HtmlElement>=None;
        for i in 0..fields.length() {
            let Some(field)=fields.item(i).and_then(|n|n.dyn_into::<web_sys::Element>().ok()) else{continue};
            if field.closest("form").ok().flatten().as_ref()!=Some(&form) {continue;}
            let valid=if let Some(input)=field.dyn_ref::<web_sys::HtmlInputElement>() {input.check_validity()}
                else if let Some(select)=field.dyn_ref::<web_sys::HtmlSelectElement>() {select.check_validity()}
                else if let Some(area)=field.dyn_ref::<web_sys::HtmlTextAreaElement>() {area.check_validity()} else{true};
            if !valid && first.is_none() {first=field.dyn_into().ok();}
        }
        if let Some(first)=first {let _=first.focus();} else {submit.emit(event);}
    }});
    html!{<form ref={node} class={classes!("ui-form",props.class.clone())} novalidate=true {onsubmit}>
        <div class="form-fields">{props.children.clone()}</div>
        <Actions>{props.actions.clone()}</Actions>
    </form>}
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Rule { #[default] Text, Required, Name, OptionalName, Path, OptionalPath, Port, HostUrl, ExecutorUrl }
fn field_error(rule:Rule,value:&str)->Option<String> {
    let raw=value;
    let value=value.trim();
    let error=match rule {
        Rule::Required | Rule::Name if value.is_empty()=>"This field is required.",
        Rule::Name | Rule::OptionalName if value.chars().count()>120=>"Use at most 120 characters.",
        Rule::Path if value.is_empty()=>"Enter an absolute directory path.",
        Rule::Path | Rule::OptionalPath if !value.is_empty()&&!raw.starts_with('/')=>"Use an absolute path starting with /.",
        Rule::Port if !value.is_empty() && raw.parse::<u16>().ok().is_none_or(|n|n==0)=>"Enter a port from 1 to 65535.",
        Rule::HostUrl if !(value.starts_with("http://")||value.starts_with("https://"))=>"Enter an http:// or https:// server URL.",
        Rule::ExecutorUrl if !(value.starts_with("ws://")||value.starts_with("wss://"))=>"Enter a ws:// or wss:// executor URL.",
        _=>return None,
    };
    Some(error.into())
}
#[derive(Properties, PartialEq)]
pub struct InputProps {
    pub value: AttrValue,
    #[prop_or_default] pub aria_label: Option<AttrValue>,
    #[prop_or_default] pub placeholder: AttrValue,
    #[prop_or_default] pub rule: Rule,
    #[prop_or_default] pub disabled: bool,
    #[prop_or_default] pub required: bool,
    #[prop_or(AttrValue::from("text"))] pub kind: AttrValue,
    #[prop_or_default] pub min: Option<AttrValue>,
    #[prop_or_default] pub max: Option<AttrValue>,
    #[prop_or_default] pub step: Option<AttrValue>,
    pub oninput: Callback<InputEvent>,
}
#[function_component(Input)]
pub fn validated_input(props:&InputProps)->Html {
    let node=use_node_ref();
    let error=use_state(||None::<String>);
    let touched=use_state(||false);
    let id=use_state(||format!("field-error-{}",uuid::Uuid::new_v4()));
    let rule=props.rule;
    let refresh=Callback::from({let node=node.clone();let error=error.clone();move |_|{
        if let Some(input)=node.cast::<web_sys::HtmlInputElement>() {
            input.set_custom_validity(field_error(rule,&input.value()).as_deref().unwrap_or(""));
            error.set(input.validation_message().ok().filter(|s|!s.is_empty()));
        }
    }});
    use_effect_with((props.value.clone(),props.rule,*touched),{let node=node.clone();let refresh=refresh.clone();move |(value,rule,touched)|{
        if let Some(input)=node.cast::<web_sys::HtmlInputElement>() {input.set_custom_validity(field_error(*rule,value).as_deref().unwrap_or(""));}
        if *touched {refresh.emit(());}
    }});
    let oninput=Callback::from({let changed=props.oninput.clone();let refresh=refresh.clone();let touched=touched.clone();move |event:InputEvent|{
        touched.set(true);refresh.emit(());changed.emit(event);
    }});
    let oninvalid=Callback::from({let touched=touched.clone();let refresh=refresh.clone();move |event:Event|{event.prevent_default();touched.set(true);refresh.emit(());}});
    let onblur=Callback::from({let touched=touched.clone();move |_:FocusEvent|{touched.set(true);refresh.emit(());}});
    html!{<><input ref={node} type={props.kind.clone()} value={props.value.clone()} placeholder={props.placeholder.clone()}
        required={props.required} disabled={props.disabled} min={props.min.clone()} max={props.max.clone()} step={props.step.clone()}
        aria-label={props.aria_label.clone()} aria-invalid={error.is_some().then_some("true")} aria-describedby={error.is_some().then(||(*id).clone())} {oninput} {oninvalid} {onblur}/>
        {if let Some(error)=&*error {html!{<span class="field-error" id={(*id).clone()} role="status">{error}</span>}}else{Html::default()}}
    </>}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_fields_and_valid_boundaries_are_accepted() {
        for rule in [Rule::Text,Rule::OptionalName,Rule::OptionalPath,Rule::Port] {
            assert_eq!(field_error(rule,""),None);
        }
        for port in ["1","22","65535"] { assert_eq!(field_error(Rule::Port,port),None); }
        assert_eq!(field_error(Rule::Path,"/work folder/project"),None);
        assert_eq!(field_error(Rule::OptionalName,&"a".repeat(120)),None);
    }
    #[test]
    fn invalid_fields_have_actionable_errors() {
        assert!(field_error(Rule::Required,"  ").unwrap().contains("required"));
        for path in ["", "relative", " /workspace"] { assert!(field_error(Rule::Path,path).is_some()); }
        for port in ["0","65536","ssh","-1"," 22"] {assert!(field_error(Rule::Port,port).is_some());}
        assert!(field_error(Rule::Name,&"a".repeat(121)).is_some());
        assert!(field_error(Rule::HostUrl,"ws://host").is_some());
        assert!(field_error(Rule::ExecutorUrl,"https://host").is_some());
    }
}
