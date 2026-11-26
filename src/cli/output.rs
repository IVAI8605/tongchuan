use serde_json::{json, Value};

/// 结构化事件：同一份数据既能渲染成旧版 key=value 文本，也能渲染成 JSON。
/// 字段名沿用现有键名，AI 与脚本看到的是同一套词汇。
#[derive(Debug, Clone)]
pub struct Event {
    pub kind: &'static str,
    pub fields: Vec<(&'static str, FieldValue)>,
}

#[derive(Debug, Clone)]
pub enum FieldValue {
    Text(String),
    Num(u64),
    Bool(bool),
}

impl Event {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            fields: Vec::new(),
        }
    }

    pub fn field(mut self, name: &'static str, value: FieldValue) -> Self {
        self.fields.push((name, value));
        self
    }

    /// 旧文本格式：`KIND k=v k2=v2`，字段名与顺序与现状一致。
    pub fn to_text(&self) -> String {
        let mut out = self.kind.to_string();
        for (k, v) in &self.fields {
            out.push(' ');
            out.push_str(k);
            out.push('=');
            match v {
                FieldValue::Text(s) => out.push_str(s),
                FieldValue::Num(n) => out.push_str(&n.to_string()),
                FieldValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            }
        }
        out
    }

    /// JSON 格式：`{"kind":"KIND","k":...}`。数字与布尔保持类型，便于 AI 直接消费。
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("kind".to_string(), json!(self.kind));
        for (k, v) in &self.fields {
            let val = match v {
                FieldValue::Text(s) => json!(s),
                FieldValue::Num(n) => json!(n),
                FieldValue::Bool(b) => json!(*b),
            };
            obj.insert((*k).to_string(), val);
        }
        Value::Object(obj)
    }

    /// 按全局输出模式选一路：text 走旧格式，json 走结构化。
    pub fn render(&self, json_mode: bool) -> String {
        if json_mode {
            self.to_json().to_string()
        } else {
            self.to_text()
        }
    }
}
