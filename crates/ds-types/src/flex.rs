//! 宽松的标量/集合反序列化辅助。
//!
//! `running.json` 里的值形态并不统一（历史原因）：
//! - `preSetIpList` 里混着 `true` / `false` / `null`
//! - `whiteList` 里是 `true`
//! - `dynamicUpstreams` 里可能是数组，也可能是单个字符串
//!
//! 这里集中处理，避免在每个结构体里写重复的 `deserialize_with`。

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 容忍 `true` / `"true"` / `1` / `null` 的布尔。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlexBool(pub bool);

impl FlexBool {
    pub const fn new(v: bool) -> Self {
        Self(v)
    }

    pub const fn get(self) -> bool {
        self.0
    }
}

impl From<bool> for FlexBool {
    fn from(v: bool) -> Self {
        Self(v)
    }
}

impl Serialize for FlexBool {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(self.0)
    }
}

impl<'de> Deserialize<'de> for FlexBool {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct FlexVisitor;

        impl<'de> Visitor<'de> for FlexVisitor {
            type Value = FlexBool;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bool / \"true\" / \"false\" / 0 / 1 / null")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
                Ok(FlexBool(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
                Ok(FlexBool(v != 0))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
                Ok(FlexBool(v != 0))
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
                Ok(FlexBool(v != 0.0))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                match v.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => Ok(FlexBool(true)),
                    "false" | "0" | "no" | "off" | "" => Ok(FlexBool(false)),
                    other => Err(E::custom(format!("无法解析为布尔: {other}"))),
                }
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(FlexBool(false))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(FlexBool(false))
            }

            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
                FlexBool::deserialize(d)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                // 空对象 `{}` 视为 false，其它形态报错
                if map.next_key::<String>()?.is_some() {
                    return Err(de::Error::custom("布尔位置出现非空对象"));
                }
                Ok(FlexBool(false))
            }
        }

        d.deserialize_any(FlexVisitor)
    }
}

/// 接受 `"a"` 或 `["a", "b"]` 的字符串列表。
pub fn de_string_or_vec<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct VecVisitor;

    impl<'de> Visitor<'de> for VecVisitor {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("字符串或字符串数组")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(vec![v.to_string()])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                out.push(item);
            }
            Ok(out)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }

    d.deserialize_any(VecVisitor)
}

/// 接受 `{"a": "x"}` 或 `{"a": ["x", "y"]}` 的映射。
pub fn de_string_or_vec_map<'de, D>(
    d: D,
) -> Result<indexmap::IndexMap<String, Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = indexmap::IndexMap::<String, serde_json::Value>::deserialize(d)?;
    let mut out = indexmap::IndexMap::with_capacity(raw.len());
    for (key, value) in raw {
        out.insert(key, value_to_string_vec(&value).map_err(de::Error::custom)?);
    }
    Ok(out)
}

fn value_to_string_vec(v: &serde_json::Value) -> Result<Vec<String>, String> {
    match v {
        serde_json::Value::String(s) => Ok(vec![s.clone()]),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::String(s) => out.push(s.clone()),
                    other => return Err(format!("需要字符串，实际为 {other}")),
                }
            }
            Ok(out)
        }
        serde_json::Value::Null => Ok(Vec::new()),
        other => Err(format!("需要字符串或字符串数组，实际为 {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_or_vec_map_accepts_mixed_forms() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(deserialize_with = "de_string_or_vec_map")]
            map: indexmap::IndexMap<String, Vec<String>>,
        }

        let h: Holder =
            serde_json::from_str(r#"{"map":{"a":"x","b":["y","z"],"c":null}}"#).unwrap();
        assert_eq!(h.map["a"], vec!["x".to_string()]);
        assert_eq!(h.map["b"], vec!["y".to_string(), "z".to_string()]);
        assert!(h.map["c"].is_empty());
    }

    #[test]
    fn flex_bool_accepts_all_forms() {
        assert!(serde_json::from_str::<FlexBool>("true").unwrap().get());
        assert!(!serde_json::from_str::<FlexBool>("false").unwrap().get());
        assert!(serde_json::from_str::<FlexBool>("\"true\"").unwrap().get());
        assert!(!serde_json::from_str::<FlexBool>("\"false\"").unwrap().get());
        assert!(serde_json::from_str::<FlexBool>("1").unwrap().get());
        assert!(!serde_json::from_str::<FlexBool>("0").unwrap().get());
        assert!(!serde_json::from_str::<FlexBool>("null").unwrap().get());
    }

    #[test]
    fn string_or_vec_accepts_both() {
        let one: Vec<String> = serde_json::from_str::<OneOrMany>("\"a\"").unwrap().0;
        let many: Vec<String> = serde_json::from_str::<OneOrMany>("[\"a\",\"b\"]").unwrap().0;
        assert_eq!(one, vec!["a".to_string()]);
        assert_eq!(many, vec!["a".to_string(), "b".to_string()]);
    }

    #[derive(Deserialize)]
    struct OneOrMany(#[serde(deserialize_with = "de_string_or_vec")] Vec<String>);
}
