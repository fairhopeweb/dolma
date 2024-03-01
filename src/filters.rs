use std::io;

use crate::shard::shard_config::FilterConfig;
use jaq_interpret::{Ctx, Filter, FilterT, ParseCtx, RcIter, Val};
use jaq_std;
use jsonpath_rust::JsonPathFinder;
use serde_json::Value;

pub struct JqDocFilter {
    pub include: Vec<Filter>,
    pub exclude: Vec<Filter>,
}

pub struct JsonPathFilter {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl JqDocFilter {
    fn parse_filters(filter_strs: Vec<String>) -> Result<Vec<Filter>, io::Error> {
        let mut defs = ParseCtx::new(Vec::new());
        defs.insert_natives(jaq_core::core());
        defs.insert_defs(jaq_std::std());
        assert!(defs.errs.is_empty());

        let mut filters: Vec<Filter> = Vec::new();
        for filter_str in filter_strs {
            let (filter, errs) = jaq_parse::parse(&filter_str, jaq_parse::main());
            if !errs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Error parsing '{:?}' into filter: {:?}", filter_str, errs),
                ));
            }
            match filter {
                Some(filter) => {
                    let filter: jaq_interpret::Filter = defs.compile(filter);
                    if !defs.errs.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("Error compiling '{:?}' into filter.", filter_str),
                        ));
                    }

                    filters.push(filter);
                }
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("Parsing '{:?}' resulted in no filter", filter_str),
                    ));
                }
            }
        }
        Ok(filters)
    }

    fn evaluate_match(&self, result: &Result<Val, jaq_interpret::Error>) -> bool {
        match result {
            Ok(jaq_interpret::Val::Bool(b)) => *b,
            Ok(jaq_interpret::Val::Null) => false,
            Ok(jaq_interpret::Val::Int(i)) => *i != 0,
            Ok(jaq_interpret::Val::Float(f)) => *f != 0.0,
            Ok(jaq_interpret::Val::Str(s)) => !s.is_empty(),
            Ok(jaq_interpret::Val::Arr(a)) => !a.is_empty(),
            Ok(jaq_interpret::Val::Obj(d)) => !d.is_empty(),
            _ => true,
        }
    }

    pub fn new(filter_config: &FilterConfig) -> Result<JqDocFilter, io::Error> {
        let include_filters = JqDocFilter::parse_filters(filter_config.include.clone())?;
        let exclude_filters = JqDocFilter::parse_filters(filter_config.exclude.clone())?;
        Ok(JqDocFilter {
            include: include_filters,
            exclude: exclude_filters,
        })
    }
    pub fn should_keep(&self, json: &Value) -> Result<bool, String> {
        let mut keep = self.include.is_empty();
        let inputs: RcIter<std::iter::Empty<_>> = RcIter::new(core::iter::empty());
        for filter in self.include.iter() {
            // exit early if keep is already true
            if keep {
                break;
            }

            let out: Vec<Result<jaq_interpret::Val, jaq_interpret::Error>> = filter
                .run((Ctx::new(Vec::new(), &inputs), Val::from(json.clone())))
                .collect();
            // if out is not empty and all its elements are true, then keep is true
            keep = !out.is_empty() && out.iter().all(|x| self.evaluate_match(x));
        }

        for filter in self.exclude.iter() {
            if !keep {
                break;
            }
            let out: Vec<_> = filter
                .run((Ctx::new(Vec::new(), &inputs), Val::from(json.clone())))
                .collect();
            keep = out.is_empty() || !out.iter().all(|x| self.evaluate_match(x));
        }
        Ok(keep)
    }
}

impl JsonPathFilter {
    pub fn new(filter_config: &FilterConfig) -> Result<JsonPathFilter, io::Error> {
        Ok(JsonPathFilter {
            include: filter_config.include.clone(),
            exclude: filter_config.exclude.clone(),
        })
    }
    pub fn should_keep(&self, json: &Value) -> Result<bool, String> {
        let mut keep = self.include.is_empty();
        for pattern in self.include.iter() {
            let mut finder = JsonPathFinder::from_str("{}", pattern)?;
            finder.set_json(Box::new(json.clone()));
            keep = finder.find() != Value::Null;
            if keep {
                break;
            }
        }
        if keep {
            for pattern in self.exclude.iter() {
                let mut finder = JsonPathFinder::from_str("{}", pattern)?;
                finder.set_json(Box::new(json.clone()));
                keep = finder.find() == Value::Null;
                if !keep {
                    break;
                }
            }
        }
        Ok(keep)
    }
}

pub struct AllowAllFilter;

impl AllowAllFilter {
    pub fn new() -> Result<AllowAllFilter, io::Error> {
        Ok(AllowAllFilter)
    }
    pub fn should_keep(&self, _json: &Value) -> Result<bool, String> {
        Ok(true)
    }
}

pub enum DocFilter {
    JqDocFilter(JqDocFilter),
    JsonPathFilter(JsonPathFilter),
    AllowAllFilter(AllowAllFilter),
}

impl DocFilter {
    pub fn new(filter_config: Option<&FilterConfig>) -> Result<DocFilter, io::Error> {
        match filter_config {
            Some(filter_config) => match filter_config.syntax.as_deref() {
                Some("jaq") => Ok(DocFilter::JqDocFilter(JqDocFilter::new(filter_config)?)),
                Some("jsonpath") | None => Ok(DocFilter::JsonPathFilter(JsonPathFilter::new(
                    filter_config,
                )?)),
                _ => Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Unknown filter syntax: {:?}", filter_config.syntax),
                )),
            },
            None => Ok(DocFilter::AllowAllFilter(AllowAllFilter::new()?)),
        }
    }
    pub fn should_keep(&self, json: &Value) -> Result<bool, String> {
        match self {
            DocFilter::JqDocFilter(f) => f.should_keep(json),
            DocFilter::JsonPathFilter(f) => f.should_keep(json),
            DocFilter::AllowAllFilter(f) => f.should_keep(json),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_should_keep() {
        let filter_config = FilterConfig {
            include: vec![".attributes.foo".to_string()],
            exclude: vec![r#".attributes.baz == "quac""#.to_string()],
            syntax: Some("jaq".to_string()),
        };
        let filters = DocFilter::new(Some(&filter_config)).unwrap();
        let doc = json!({
            "attributes": {
                "foo": "bar",
                "baz": "qux"
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), true);
    }

    #[test]
    fn test_should_remove() {
        let filter_config = FilterConfig {
            include: vec![".attributes.foo".to_string()],
            exclude: vec![r#".attributes.baz == "qux""#.to_string()],
            syntax: Some("jaq".to_string()),
        };
        let filters = DocFilter::new(Some(&filter_config)).unwrap();
        let doc = json!({
            "attributes": {
                "foo": "bar",
                "baz": "qux"
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), false);
    }

    #[test]
    fn test_aggregate_filters() {
        let filter_config = FilterConfig {
            include: vec![".attributes.foo | length >= 3".to_string()],
            exclude: vec![],
            syntax: Some("jaq".to_string()),
        };
        let filters = DocFilter::new(Some(&filter_config)).unwrap();
        let doc = json!({
            "attributes": {
                "foo": [1.0, 2.0, 3.0],
                "baz": [4.0, 5.0]
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), true);
    }

    #[test]
    fn test_allow_all() {
        let filters = DocFilter::new(None).unwrap();
        let doc = json!({
            "attributes": {
                "foo": [1.0, 2.0, 3.0],
                "baz": [4.0, 5.0]
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), true);
    }

    #[test]
    fn test_jsonpath_allow() {
        let filter_config = FilterConfig {
            include: vec!["$..foo".to_string()],
            exclude: vec![],
            syntax: Some("jsonpath".to_string()),
        };
        let filters = DocFilter::new(Some(&filter_config)).unwrap();
        let doc = json!({
            "foo": "bar",
            "baz": "qux"
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), true);
    }

    #[test]
    fn test_jsonpath_exclude() {
        let filter_config = FilterConfig {
            include: vec![],
            exclude: vec![
                "$@.attributes[?(@.value1 && @.value1[0] && @.value1[0][2] >= 1.0)]".to_string(),
            ],
            syntax: Some("jsonpath".to_string()),
        };
        let filters = DocFilter::new(Some(&filter_config)).unwrap();
        let doc = json!({
            "attributes": {
                "value1": [[0, 30, 1.0], [30, 45, 0.5]],
                "value2": [[45, 60, 0.0], [60, 75, 0.5]],
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), false);

        let doc = json!({
            "attributes": {
                "value1": [[0, 30, 1], [30, 45, 0]],
                "value2": [[45, 60, 0], [60, 75, 0]],
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), true);
    }

    #[test]
    fn test_sum_jq_filter() {
        let filter_config = FilterConfig {
            include: vec![".attributes.foo | add >= 6".to_string()],
            exclude: vec![],
            syntax: Some("jaq".to_string()),
        };
        let filters = DocFilter::new(Some(&filter_config)).unwrap();
        let doc = json!({
            "attributes": {
                "foo": [1.0, 2.0, 3.0],
                "baz": [4.0, 5.0]
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), true);

        let doc = json!({
            "attributes": {
                "foo": [1.0, 2.0, 1.0],
                "baz": [4.0, 5.0]
            }
        });
        assert_eq!(filters.should_keep(&doc).unwrap(), false);
    }

    #[test]
    fn test_jq_raise_errror_compile() {
        let filter_config = FilterConfig {
            include: vec![".x | sum".to_string()],
            exclude: vec![],
            syntax: Some("jaq".to_string()),
        };

        let result = DocFilter::new(Some(&filter_config));
        assert!(result.is_err());
    }
}
