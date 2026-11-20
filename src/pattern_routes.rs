//! Immutable, engine-owned pattern dispatch. Scans borrow keys and route slices.
use std::collections::HashMap;

use crate::pattern::{Kind, Signature};

#[derive(Default)]
pub(crate) struct Index {
    routes: HashMap<Kind, HashMap<String, Bucket>>,
}

#[derive(Default)]
struct Bucket {
    generic: Vec<usize>,
    specific: HashMap<Kind, HashMap<String, Vec<usize>>>,
}

pub(crate) struct Candidates<'a> {
    pub(crate) specific: &'a [usize],
    pub(crate) generic: &'a [usize],
}

impl Candidates<'_> {
    /// Merge sorted, unique buckets in linear time, preserving index order.
    pub(crate) fn indices(mut self) -> impl Iterator<Item = usize> {
        std::iter::from_fn(move || {
            let next = match (self.specific.first(), self.generic.first()) {
                (Some(a), Some(b)) => (*a).min(*b),
                (Some(a), None) | (None, Some(a)) => *a,
                (None, None) => return None,
            };
            if self.specific.first() == Some(&next) {
                self.specific = &self.specific[1..];
            }
            if self.generic.first() == Some(&next) {
                self.generic = &self.generic[1..];
            }
            Some(next)
        })
    }
}

impl Index {
    pub(crate) fn add(&mut self, signature: Signature<'_>, rule: usize) {
        let names = self.routes.entry(signature.kind).or_default();
        let bucket = names.entry(signature.name.to_owned()).or_default();
        let indices = match signature.module {
            Some((kind, name)) => {
                let modules = bucket.specific.entry(kind).or_default();
                modules.entry(name.to_owned()).or_default()
            }
            None => &mut bucket.generic,
        };
        // Construction normally visits increasing indices. Keep the invariant
        // explicit even when callers add a duplicate or insert out of order.
        if let Err(position) = indices.binary_search(&rule) {
            indices.insert(position, rule);
        }
    }

    pub(crate) fn candidates(&self, signature: Signature<'_>) -> Candidates<'_> {
        let bucket = self
            .routes
            .get(&signature.kind)
            .and_then(|names| names.get(signature.name));
        let generic = bucket.map_or(&[][..], |bucket| bucket.generic.as_slice());
        let specific = bucket
            .and_then(|bucket| {
                signature.module.and_then(|(kind, name)| {
                    bucket
                        .specific
                        .get(&kind)
                        .and_then(|modules| modules.get(name))
                })
            })
            .map_or(&[][..], Vec::as_slice);
        Candidates { specific, generic }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature<'a>(kind: Kind, name: &'a str, module: Option<(Kind, &'a str)>) -> Signature<'a> {
        Signature { kind, name, module }
    }

    #[test]
    fn routes_many_modules_by_literal_name_and_kind() {
        let mut index = Index::default();
        for n in 0..100 {
            let module = format!("Module{n}");
            index.add(
                signature(Kind::RemoteCall, "map", Some((Kind::Alias, &module))),
                n,
            );
        }
        index.add(
            signature(Kind::RemoteCall, "map", Some((Kind::Atom, "Module42"))),
            100,
        );
        index.add(
            signature(Kind::RemoteCall, "map", Some((Kind::Context, "Module42"))),
            101,
        );
        for (kind, expected) in [(Kind::Alias, 42), (Kind::Atom, 100), (Kind::Context, 101)] {
            let found =
                index.candidates(signature(Kind::RemoteCall, "map", Some((kind, "Module42"))));
            assert_eq!(found.specific, &[expected]);
            assert!(found.generic.is_empty());
        }
        assert!(
            index
                .candidates(signature(
                    Kind::RemoteCall,
                    "map",
                    Some((Kind::Alias, "Missing"))
                ))
                .specific
                .is_empty()
        );
    }

    #[test]
    fn returns_generic_and_specific_separately() {
        let mut index = Index::default();
        index.add(signature(Kind::RemoteCall, "map", None), 1);
        index.add(
            signature(Kind::RemoteCall, "map", Some((Kind::Alias, "Enum"))),
            2,
        );
        index.add(
            signature(Kind::RemoteCall, "map", Some((Kind::Alias, "Enum"))),
            3,
        );
        let found = index.candidates(signature(
            Kind::RemoteCall,
            "map",
            Some((Kind::Alias, "Enum")),
        ));
        assert_eq!(found.generic, &[1]);
        assert_eq!(found.specific, &[2, 3]);
        let found = index.candidates(signature(Kind::RemoteCall, "map", None));
        assert_eq!(found.generic, &[1]);
        assert!(found.specific.is_empty());
    }

    #[test]
    fn unrelated_callees_and_call_kinds_are_empty() {
        let mut index = Index::default();
        index.add(signature(Kind::RemoteCall, "map", None), 1);
        index.add(
            signature(Kind::RemoteCall, "map", Some((Kind::Alias, "Enum"))),
            2,
        );
        for sig in [
            signature(Kind::RemoteCall, "filter", Some((Kind::Alias, "Enum"))),
            signature(Kind::LocalCall, "map", Some((Kind::Alias, "Enum"))),
        ] {
            let found = index.candidates(sig);
            assert!(found.generic.is_empty());
            assert!(found.specific.is_empty());
        }
    }

    #[test]
    fn merges_overlapping_buckets_once_in_index_order() {
        let mut index = Index::default();
        let generic = signature(Kind::RemoteCall, "map", None);
        let specific = signature(Kind::RemoteCall, "map", Some((Kind::Alias, "Enum")));
        for n in (0..1024).rev() {
            index.add(generic, n);
            index.add(generic, n);
            if n % 2 == 0 {
                index.add(specific, n);
            }
        }
        assert_eq!(
            index.candidates(specific).indices().collect::<Vec<_>>(),
            (0..1024).collect::<Vec<_>>()
        );
        assert_eq!(
            Candidates {
                specific: &[],
                generic: &[]
            }
            .indices()
            .next(),
            None
        );
    }
}
