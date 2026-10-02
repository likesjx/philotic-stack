//! RFC 6901 JSON Pointer read/write for surface data models. serde_json can read
//! a pointer but has no insert or remove, which `updateDataModel` needs.

use serde_json::{Map, Value};

/// Split a pointer into unescaped reference tokens. `""` and `"/"` address the
/// whole document (A2UI treats an omitted path and `/` as the root).
pub fn tokens(pointer: &str) -> Result<Vec<String>, String> {
    if pointer.is_empty() || pointer == "/" {
        return Ok(Vec::new());
    }
    let rest = pointer
        .strip_prefix('/')
        .ok_or_else(|| format!("JSON Pointer must start with '/': {pointer}"))?;
    rest.split('/').map(unescape).collect()
}

fn unescape(token: &str) -> Result<String, String> {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return Err(format!("invalid '~' escape in JSON Pointer token {token}")),
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// Replace or create the value at `pointer`, creating intermediate objects.
/// An array index may name an existing element or `-` / the length to append.
pub fn set(doc: &mut Value, pointer: &str, value: Value) -> Result<(), String> {
    let tokens = tokens(pointer)?;
    let Some((last, parents)) = tokens.split_last() else {
        *doc = value;
        return Ok(());
    };
    let mut current = doc;
    for token in parents {
        if current.is_null() {
            *current = Value::Object(Map::new());
        }
        current = match current {
            Value::Object(map) => map
                .entry(token.clone())
                .or_insert_with(|| Value::Object(Map::new())),
            Value::Array(items) => {
                let index = array_index(token, items.len(), false)?;
                &mut items[index]
            }
            _ => return Err(format!("cannot descend into a scalar at '{token}'")),
        };
    }
    if current.is_null() {
        *current = Value::Object(Map::new());
    }
    match current {
        Value::Object(map) => {
            map.insert(last.clone(), value);
            Ok(())
        }
        Value::Array(items) => {
            let index = array_index(last, items.len(), true)?;
            if index == items.len() {
                items.push(value);
            } else {
                items[index] = value;
            }
            Ok(())
        }
        _ => Err(format!("cannot set '{last}' on a scalar")),
    }
}

/// Remove the value at `pointer`. Removing a missing key is not an error; the
/// result is the same model either way.
pub fn remove(doc: &mut Value, pointer: &str) -> Result<(), String> {
    let tokens = tokens(pointer)?;
    let Some((last, parents)) = tokens.split_last() else {
        *doc = Value::Object(Map::new());
        return Ok(());
    };
    let mut current = doc;
    for token in parents {
        current = match current {
            Value::Object(map) => match map.get_mut(token) {
                Some(next) => next,
                None => return Ok(()),
            },
            Value::Array(items) => match array_index(token, items.len(), false) {
                Ok(index) => &mut items[index],
                Err(_) => return Ok(()),
            },
            _ => return Ok(()),
        };
    }
    match current {
        Value::Object(map) => {
            map.remove(last);
        }
        Value::Array(items) => {
            if let Ok(index) = array_index(last, items.len(), false) {
                items.remove(index);
            }
        }
        _ => {}
    }
    Ok(())
}

fn array_index(token: &str, len: usize, allow_append: bool) -> Result<usize, String> {
    if allow_append && token == "-" {
        return Ok(len);
    }
    let leading_zero = token.len() > 1 && token.starts_with('0');
    let index: usize = token
        .parse()
        .ok()
        .filter(|_| !leading_zero)
        .ok_or_else(|| format!("invalid array index '{token}'"))?;
    let max = if allow_append {
        len
    } else {
        len.saturating_sub(1)
    };
    if index > max || (!allow_append && len == 0) {
        return Err(format!("array index {index} out of bounds (len {len})"));
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn root_pointers_replace_the_document() {
        let mut doc = json!({"a": 1});
        set(&mut doc, "/", json!({"b": 2})).unwrap();
        assert_eq!(doc, json!({"b": 2}));
        set(&mut doc, "", json!([1])).unwrap();
        assert_eq!(doc, json!([1]));
    }

    #[test]
    fn set_creates_intermediate_objects() {
        let mut doc = Value::Null;
        set(&mut doc, "/user/name", json!("Jared")).unwrap();
        assert_eq!(doc, json!({"user": {"name": "Jared"}}));
    }

    #[test]
    fn escapes_follow_rfc_6901() {
        let mut doc = json!({});
        set(&mut doc, "/a~1b/c~0d", json!(1)).unwrap();
        assert_eq!(doc, json!({"a/b": {"c~d": 1}}));
        assert_eq!(doc.pointer("/a~1b/c~0d"), Some(&json!(1)));
        assert!(tokens("/bad~2").is_err());
        assert!(tokens("no-slash").is_err());
    }

    #[test]
    fn arrays_replace_and_append() {
        let mut doc = json!({"items": [1, 2]});
        set(&mut doc, "/items/0", json!(9)).unwrap();
        set(&mut doc, "/items/-", json!(3)).unwrap();
        set(&mut doc, "/items/3", json!(4)).unwrap();
        assert_eq!(doc, json!({"items": [9, 2, 3, 4]}));
        assert!(set(&mut doc, "/items/9", json!(0)).is_err());
        assert!(set(&mut doc, "/items/01", json!(0)).is_err());
    }

    #[test]
    fn remove_deletes_and_tolerates_missing() {
        let mut doc = json!({"a": {"b": 1, "c": 2}, "list": [1, 2, 3]});
        remove(&mut doc, "/a/b").unwrap();
        remove(&mut doc, "/list/1").unwrap();
        remove(&mut doc, "/missing/deep").unwrap();
        assert_eq!(doc, json!({"a": {"c": 2}, "list": [1, 3]}));
    }
}
