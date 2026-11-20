//! Load-scoped sharing of immutable compiled queries. No source or scan results are cached.
use crate::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tree_sitter::Query;

type Compiled = Result<Arc<Query>>;

/// Shared by parallel rule loaders. One initialization per distinct query text,
/// including failures. The cache is dropped after the engine has been loaded;
/// each matcher retains only the compiled queries it actually uses.
#[derive(Default)]
pub(crate) struct QueryCompiler {
    queries: Mutex<HashMap<String, Arc<OnceLock<Compiled>>>>,
    #[cfg(test)]
    compilations: std::sync::atomic::AtomicUsize,
}

impl QueryCompiler {
    pub(crate) fn compile(&self, text: &str, finding: bool) -> Result<(Arc<Query>, Option<u32>)> {
        let entry = {
            let mut queries = self
                .queries
                .lock()
                .map_err(|_| "query compiler lock poisoned")?;
            queries.get(text).cloned().unwrap_or_else(|| {
                let entry = Arc::new(OnceLock::new());
                queries.insert(text.to_owned(), Arc::clone(&entry));
                entry
            })
        };
        // Do not hold the map lock during compilation. Different query texts
        // compile concurrently, while identical requests wait for the same result.
        let query = entry
            .get_or_init(|| {
                #[cfg(test)]
                self.compilations
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                compile(text).map(Arc::new)
            })
            .clone()?;
        let capture = query.capture_index_for_name("finding");
        // Requirements belong to the caller, not to the cached native query.
        if finding && capture.is_none() {
            return Err("query needs a @finding capture".into());
        }
        Ok((query, capture))
    }
}

fn compile(text: &str) -> Result<Query> {
    let query =
        Query::new(&crate::engine::language(), text).map_err(|e| format!("invalid query: {e}"))?;
    for index in 0..query.pattern_count() {
        if !query.property_settings(index).is_empty()
            || !query.property_predicates(index).is_empty()
        {
            return Err("query directives (#set!, #is?) are not supported".into());
        }
        for predicate in query.general_predicates(index) {
            let numeric = (crate::engine::COUNT_PREDICATES.contains(&predicate.operator.as_ref())
                || matches!(
                    predicate.operator.as_ref(),
                    "line-span-gt?" | "list-items-gt?"
                )
                || crate::scan::sibling_direction(&predicate.operator).is_some())
                && matches!(
                    predicate.args.as_ref(),
                    [tree_sitter::QueryPredicateArg::Capture(_), tree_sitter::QueryPredicateArg::String(n)]
                        if n.parse::<usize>().is_ok()
                );
            let literal = crate::literal::PREDICATES.contains(&predicate.operator.as_ref())
                && matches!(
                    predicate.args.first(),
                    Some(tree_sitter::QueryPredicateArg::Capture(_))
                )
                && predicate.args.len() >= 2
                && (predicate.operator.ends_with("any-of?") || predicate.args.len() == 2)
                && predicate.args[1..]
                    .iter()
                    .all(|arg| matches!(arg, tree_sitter::QueryPredicateArg::String(_)));
            if !numeric && !literal {
                return Err(format!(
                    "unsupported predicate #{}; use #eq?, #match?, #any-of?, their not- forms, \
                      #name-eq?/#name-any-of?, #atom-eq?/#atom-any-of?, \
                       #count-eq?/#count-gt?/#count-lt?, #line-span-gt?, \
                       #list-items-gt?, or #preceding-count-eq?/#following-count-eq? @capture N",
                    predicate.operator
                ));
            }
        }
    }
    Ok(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;
    use std::sync::atomic::Ordering::Relaxed;

    #[test]
    fn parallel_requests_share_one_compilation_and_release_with_the_engine() {
        let compiler = QueryCompiler::default();
        let queries: Vec<_> = (0..64)
            .into_par_iter()
            .map(|_| compiler.compile("(identifier) @finding", true).unwrap().0)
            .collect();
        assert_eq!(compiler.compilations.load(Relaxed), 1);
        assert!(queries.iter().all(|q| Arc::ptr_eq(q, &queries[0])));
        let weak = Arc::downgrade(&queries[0]);
        drop(compiler);
        assert!(weak.upgrade().is_some());
        drop(queries);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn caller_requirements_are_checked_on_hits_and_failures_remain_errors() {
        let compiler = QueryCompiler::default();
        let first = compiler.compile("(identifier) @_value", false).unwrap().0;
        assert!(
            compiler
                .compile("(identifier) @_value", true)
                .unwrap_err()
                .contains("@finding")
        );
        assert!(Arc::ptr_eq(
            &first,
            &compiler.compile("(identifier) @_value", false).unwrap().0
        ));
        for _ in 0..3 {
            assert!(compiler.compile("(not_a_node) @finding", false).is_err());
            assert!(
                compiler
                    .compile("((identifier) @finding (#unknown? @finding))", false)
                    .is_err()
            );
        }
        assert_eq!(compiler.compilations.load(Relaxed), 3);
    }
}
