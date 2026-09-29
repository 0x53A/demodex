//! Escaped unified diff lines; malformed headers remain visible without invented numbers.
use yew::prelude::*;

fn lines(diff: &str) -> Vec<(String, String, &'static str, &str)> {
    fn range(value: &str) -> Option<(u64, u64)> {
        let (start, count) = value.split_once(',').unwrap_or((value, "1"));
        Some((start.parse().ok()?, count.parse().ok()?))
    }
    fn header(line: &str) -> Option<((u64, u64), (u64, u64))> {
        let (ranges, _) = line.strip_prefix("@@ -")?.split_once(" @@")?;
        let (old, new) = ranges.split_once(" +")?;
        Some((range(old)?, range(new)?))
    }
    fn number(side: &mut Option<(u64, u64)>) -> String {
        let Some((line, remaining)) = *side else { return String::new(); };
        if remaining == 0 { return String::new(); }
        *side = line.checked_add(1).map(|next| (next, remaining - 1));
        line.to_string()
    }
    let (mut old, mut new) = (None::<(u64, u64)>, None::<(u64, u64)>);
    diff.lines().map(|line| {
        if old.is_none_or(|(_, left)| left == 0) && new.is_none_or(|(_, left)| left == 0) {
            old = None;
            new = None;
        }
        let mut numbers = (String::new(), String::new());
        let class = if line.starts_with("@@") {
            (old, new) = header(line).map(|(a,b)| (Some(a),Some(b))).unwrap_or_default();
            "diff-hunk"
        } else if ((line.starts_with("--- ") || line.starts_with("+++ ")) && old.is_none() && new.is_none()) || line.starts_with("diff ") {
            old=None; new=None; "diff-meta"
        } else {
            let prefix = line.chars().next();
            if matches!(prefix, Some('-'|' ')) {
                numbers.0 = number(&mut old);
            }
            if matches!(prefix, Some('+'|' ')) {
                numbers.1 = number(&mut new);
            }
            match prefix {Some('+')=>"diff-add",Some('-')=>"diff-delete",_=>"diff-context"}
        };
        (numbers.0,numbers.1,class,line)
    }).collect()
}

pub fn view(diff: &str) -> Html {
    html!{<pre class="diff-view">{for lines(diff).into_iter().map(|(old,new,class,line)|html!{
        <span class={classes!("diff-line",class)}><span class="diff-number" aria-hidden="true">{old}</span><span class="diff-number" aria-hidden="true">{new}</span><code>{line}{"\n"}</code></span>
    })}</pre>}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_headers_and_malformed_hunks_do_not_invent_line_numbers() {
        let rows = lines("@@ -3 +7 @@\n--- content\n+++ content\n--- a/next\n+++ b/next\n@@ -8 +9 broken\n-a\n+b\n@@ -0,0 +1 @@\n+new");
        assert_eq!(rows[1], ("3".into(), "".into(), "diff-delete", "--- content"));
        assert_eq!(rows[2].1, "7");
        for i in [3,4,6,7] {
            assert!(rows[i].0.is_empty() && rows[i].1.is_empty());
        }
        assert_eq!(rows[3].2, "diff-meta");
        assert_eq!(rows[4].2, "diff-meta");
        assert_eq!(rows[9].1, "1");
    }
    #[test]
    fn hunk_numbers_follow_each_side_and_preserve_text() {
        let rows=lines("--- a/file\n+++ b/file\n@@ -3,2 +7,2 @@\n same\n-<script>\n+🦆\n\\ No newline at end of file\n@@ -90 +100 @@\n-x\n+y");
        assert_eq!((&rows[3].0,&rows[3].1),(&"3".into(),&"7".into()));
        assert_eq!(rows[4],("4".into(),"".into(),"diff-delete","-<script>"));
        assert_eq!(rows[5],("".into(),"8".into(),"diff-add","+🦆"));
        assert_eq!(rows[8].0,"90");
        assert_eq!(rows[9].1,"100");
    }
}
