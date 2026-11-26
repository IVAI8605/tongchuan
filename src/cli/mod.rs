pub mod output;
pub mod spec;

#[cfg(test)]
mod tests {
    use super::output::{Event, FieldValue};
    use super::spec::{commands, EXPECTED_NAMES};

    #[test]
    fn spec_exposes_all_thirteen_commands() {
        let cmds = commands();
        assert_eq!(cmds.len(), 13, "现有 13 个命令必须全部登记");
        for name in EXPECTED_NAMES {
            assert!(
                cmds.iter().any(|c| c.name == *name),
                "spec 缺少命令: {name}"
            );
        }
    }

    #[test]
    fn spec_uses_real_history_flag_not_usage_typo() {
        let history = commands().iter().find(|c| c.name == "history").unwrap();
        assert!(
            history.args.iter().any(|a| a.name == "--history-dir"),
            "history 代码读的是 --history-dir，不能照抄 usage 里的 --dir 笔误"
        );
        assert!(
            !history.args.iter().any(|a| a.name == "--dir"),
            "不能把 usage 笔误 --dir 固化进 spec"
        );
    }

    #[test]
    fn event_renders_legacy_text_format() {
        let e = Event::new("SEND")
            .field("txn", FieldValue::Text("abc".into()))
            .field("files", FieldValue::Num(3))
            .field("verdict", FieldValue::Text("OK".into()));
        assert_eq!(e.to_text(), "SEND txn=abc files=3 verdict=OK");
    }

    #[test]
    fn event_renders_typed_json_with_kind() {
        let e = Event::new("SEND")
            .field("txn", FieldValue::Text("abc".into()))
            .field("files", FieldValue::Num(3))
            .field("ok", FieldValue::Bool(true));
        let v = e.to_json();
        assert_eq!(v["kind"], "SEND");
        assert_eq!(v["txn"], "abc");
        assert_eq!(v["files"], 3);
        assert_eq!(v["ok"], true);
        assert!(v.is_object());
    }
}
