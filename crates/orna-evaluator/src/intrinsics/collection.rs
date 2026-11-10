//! Standard-library dispatch for the collection family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    pub(crate) fn collection(
        &mut self,
        name: &str,
        values: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("recursive_cte" | "__recursive_cte", [Value::List(anchor), recursive_term]) => {
                self.recursive_cte(anchor, recursive_term, depth)
            }
            ("__list_length", [Value::List(values)]) => self.count(values),
            ("__list_concat", [Value::List(left), Value::List(right)]) => self.union(left, right),
            ("__numeric_sum", [Value::List(values)]) => self.sum(values),
            ("__stable_sort", [Value::List(values), key]) => self.sort_by(values, key, depth),
            ("__minimum", [Value::List(values)]) => self.extreme("min", values),
            ("__maximum", [Value::List(values)]) => self.extreme("max", values),
            ("__group_by", [Value::List(values), key]) => self.group_by(values, key, depth),
            ("__rank", [Value::List(values), key]) => self.rank(values, key, depth),
            ("__asof_join", [Value::List(left), Value::List(right), time, by]) => {
                self.asof_join(left, right, time, by, depth)
            }
            ("bucket_by" | "__bucket_by", [Value::List(rows), period, zone]) => {
                self.collection_bucket_by(rows, period, zone)
            }
            ("distinct", [Value::List(values)]) => self.distinct(values),
            ("union", [Value::List(left), Value::List(right)]) => self.union(left, right),
            ("count", [Value::List(values)]) => self.count(values),
            ("first", [Value::List(values)]) => self.first(values),
            ("last", [Value::List(values)]) => self.last(values),
            ("min" | "max", [Value::List(values)]) => self.extreme(name, values),
            ("sum", [Value::List(values)]) => self.sum(values),
            ("one", [Value::List(values)]) => self.one(values, None, depth),
            ("one", [Value::List(values), predicate]) => self.one(values, Some(predicate), depth),
            ("every", [Value::List(values), predicate]) => self.every(values, predicate, depth),
            ("exists", [Value::List(values), predicate]) => self.exists(values, predicate, depth),
            ("take", [Value::List(values), Value::Int(count)]) => self.take(values, count),
            ("drop", [Value::List(values), Value::Int(count)]) => self.drop(values, count),
            ("map", [Value::List(values), transform]) => self.map(values, transform, depth),
            ("flat_map", [Value::List(values), transform]) => {
                self.flat_map(values, transform, depth)
            }
            ("sort_by", [Value::List(values), key]) => self.sort_by(values, key, depth),
            ("filter", [Value::List(values), predicate]) => self.filter(values, predicate, depth),
            ("group_by", [Value::List(values), key]) => self.group_by(values, key, depth),
            ("asof_join", [Value::List(left), Value::List(right), time, by]) => {
                self.asof_join(left, right, time, by, depth)
            }
            ("__strictly_ordered_time_series", [Value::List(points)]) => {
                self.strictly_ordered_time_series(points)
            }
            ("__window_rate", [Value::List(points), Value::Int(size), Value::Int(step)]) => {
                self.window_time_series_statistics(points, size, step, "rate")
            }
            (
                "__window_rate_integrate",
                [Value::List(points), Value::Int(size), Value::Int(step)],
            ) => self.window_time_series_statistics(points, size, step, "rate_integrate"),
            ("__window_derivative", [Value::List(points), Value::Int(size), Value::Int(step)]) => {
                self.window_time_series_statistics(points, size, step, "derivative")
            }
            ("__window_integrate", [Value::List(points), Value::Int(size), Value::Int(step)]) => {
                self.window_time_series_statistics(points, size, step, "integrate")
            }
            _ => Self::collection_shape_error(name, values.as_slice()),
        }
    }
    /// Groups finite rows by a bucket period, charging one step per row and
    /// one item budget per retained bucket.
    fn collection_bucket_by(
        &mut self,
        rows: &List,
        period: &Value,
        zone: &Value,
    ) -> Result<Value, EvaluationError> {
        let spec = bucket_by_spec(period, Some(zone))?;
        let mut state = RelationBucketState::try_new(spec).map_err(bucket_error)?;
        self.items(rows.len())?;
        let mut groups = Vec::new();
        for row in rows {
            self.step()?;
            if let Some(bucket) = state.push(row.clone()).map_err(bucket_error)? {
                self.items(bucket.values.len())?;
                groups.push(Value::list(bucket.values));
                self.items(groups.len())?;
            }
        }
        if let Some(bucket) = state.finish() {
            self.items(bucket.values.len())?;
            groups.push(Value::list(bucket.values));
            self.items(groups.len())?;
        }
        Ok(Value::list(groups))
    }
    /// Rejects a known collection spelling whose argument shape does not fit,
    /// keeping the type error distinct from the unsupported fallback.
    fn collection_shape_error(name: &str, values: &[Value]) -> Result<Value, EvaluationError> {
        match (name, values) {
            ("distinct" | "count" | "first" | "last" | "min" | "max" | "sum", [_])
            | ("one", [_] | [_, _])
            | ("every" | "exists", [_, _])
            | ("count", [_, _])
            | ("union", [_, _])
            | ("take", [_, _])
            | ("drop", [_, _])
            | ("map", [_, _])
            | ("flat_map", [_, _])
            | ("sort_by", [_, _])
            | ("filter", [_, _])
            | ("group_by", [_, _])
            | ("asof_join", [_, _, _, _])
            | ("__bucket_by", [_, _, _]) => Err(error("ORNA-EVAL-TYPE")),
            ("__strictly_ordered_time_series", [_]) => Err(error("ORNA-EVAL-TYPE")),
            (
                "__window_rate"
                | "__window_rate_integrate"
                | "__window_derivative"
                | "__window_integrate",
                [_, _, _],
            ) => Err(error("ORNA-EVAL-TYPE")),
            ("__list_length" | "__numeric_sum", [_])
            | ("__list_concat", [_, _])
            | ("__stable_sort" | "__group_by" | "__rank", [_, _])
            | ("__minimum" | "__maximum", [_])
            | ("__asof_join", [_, _, _, _]) => Err(error("ORNA-EVAL-TYPE")),
            ("recursive_cte" | "__recursive_cte", [_, _]) => Err(error("ORNA-EVAL-TYPE")),
            ("asof_join", _) => Err(error("ORNA-EVAL-ARGUMENT")),
            ("recursive_cte" | "__recursive_cte", _) => Err(error("ORNA-EVAL-ARGUMENT")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    /// Computes a finite breadth-first recursive query, using one canonical
    /// identity set for the anchor and every recursive round. The invocation
    /// owns that fold; input anchor order (including any prior lateral
    /// flat-map order) determines first values and the breadth-first output
    /// order. The returned order is preserved by downstream window frames;
    /// sibling calls keep separate folds even when those frames overlap in
    /// value identity.
    fn recursive_cte(
        &mut self,
        anchor: &[Value],
        recursive_term: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let mut fold = DistinctValueFold::default();
        let mut frontier = Vec::new();
        self.items(anchor.len())?;
        for value in anchor {
            self.step()?;
            if fold.insert(value.clone())? {
                frontier.push(value.clone());
                self.items(fold.len())?;
            }
        }

        while !frontier.is_empty() {
            let mut next_frontier = Vec::new();
            for row in std::mem::take(&mut frontier) {
                self.step()?;
                let next = self.invoke_predicate(recursive_term, row, depth + 1)?;
                let Value::List(candidates) = next else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.items(candidates.len())?;
                for value in candidates {
                    self.step()?;
                    if fold.insert(value.clone())? {
                        next_frontier.push(value);
                        self.items(fold.len())?;
                        self.items(next_frontier.len())?;
                    }
                }
            }
            frontier = next_frontier;
        }

        Ok(Value::list(fold.into_values()))
    }
    fn map(
        &mut self,
        values: &[Value],
        transform: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut mapped = Vec::with_capacity(values.len());
        for value in values {
            mapped.push(self.invoke_predicate(transform, value.clone(), depth + 1)?);
            self.items(mapped.len())?;
        }
        Ok(Value::list(mapped))
    }
    fn flat_map(
        &mut self,
        values: &[Value],
        transform: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut flattened = Vec::new();
        for value in values {
            let Value::List(inner) = self.invoke_predicate(transform, value.clone(), depth + 1)?
            else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            self.items(inner.len())?;
            for value in inner {
                flattened.push(value);
                self.items(flattened.len())?;
            }
        }
        Ok(Value::list(flattened))
    }
    fn sort_by(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut keys = Vec::with_capacity(values.len());
        for value in values {
            // Evaluate every callback in source order before considering any
            // reordering. This preserves effect/error observability even when
            // the eventual sort can be optimized by its caller.
            let sort_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_sort_key(&sort_key)?;
            keys.push(sort_key);
            self.items(keys.len())?;
        }
        let order = self.stable_order(&keys)?;
        Ok(Value::List(
            order
                .into_iter()
                .map(|index| values[index].clone())
                .collect(),
        ))
    }
    fn rank(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut keys = Vec::with_capacity(values.len());
        for value in values {
            // Callback evaluation is observable and must consume one bounded
            // step even when the callback body is only an identity lookup.
            self.step()?;
            let sort_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_sort_key(&sort_key)?;
            keys.push(sort_key);
            self.items(keys.len())?;
        }
        let order = self.stable_order(&keys)?;
        let mut ranked = Vec::with_capacity(order.len());
        let mut competition_rank = 1usize;
        for (position, &index) in order.iter().enumerate() {
            if position > 0 {
                self.step()?;
                if compare_sort_keys(&keys[order[position - 1]], &keys[index])?
                    != std::cmp::Ordering::Equal
                {
                    competition_rank = position
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                }
            }
            let rank = self.integer(BigInt::from(competition_rank))?;
            ranked.push(Value::Tuple(vec![values[index].clone(), Value::Int(rank)]));
            self.items(ranked.len())?;
        }
        Ok(Value::list(ranked))
    }
    /// Stable merge sort of `0..keys.len()` by key. Each comparison costs one
    /// step and incomparable keys fail explicitly; equal keys keep source
    /// order. O(n log n) comparisons replace the former insertion sorts.
    fn stable_order(&mut self, keys: &[Value]) -> Result<Vec<usize>, EvaluationError> {
        let mut order = (0..keys.len()).collect::<Vec<_>>();
        let mut merged = Vec::with_capacity(order.len());
        let mut width = 1;
        while width < order.len() {
            merged.clear();
            for start in (0..order.len()).step_by(width * 2) {
                let middle = (start + width).min(order.len());
                let end = (start + width * 2).min(order.len());
                let (mut left, mut right) = (start, middle);
                while left < middle && right < end {
                    self.step()?;
                    if compare_sort_keys(&keys[order[right]], &keys[order[left]])?
                        == std::cmp::Ordering::Less
                    {
                        merged.push(order[right]);
                        right += 1;
                    } else {
                        merged.push(order[left]);
                        left += 1;
                    }
                }
                merged.extend_from_slice(&order[left..middle]);
                merged.extend_from_slice(&order[right..end]);
            }
            std::mem::swap(&mut order, &mut merged);
            width *= 2;
        }
        Ok(order)
    }
    fn distinct(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut fold = DistinctValueFold::default();
        for value in values {
            self.step()?;
            if !fold.insert(value.clone())? {
                continue;
            }
            self.step()?;
            self.items(fold.len())?;
        }
        Ok(Value::list(fold.into_values()))
    }
    /// Concatenates `left` then `right`. Appending at the end of the longest
    /// view of a list reuses its storage, so the charge covers only the
    /// elements actually written: the appended ones and any copied prefix.
    fn union(&mut self, left: &List, right: &[Value]) -> Result<Value, EvaluationError> {
        self.items(left.len())?;
        self.items(right.len())?;
        let length = left
            .len()
            .checked_add(right.len())
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.items(length)?;
        let (values, copied) = left.concat(right);
        for _ in 0..copied + right.len() {
            self.step()?;
        }
        Ok(Value::List(values))
    }
    fn count(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        // A finite list knows its length: one bounded step, not one per item.
        self.step()?;
        Ok(Value::Int(BigInt::from(values.len())))
    }
    pub(crate) fn sum(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        if values.iter().all(|value| matches!(value, Value::Float(_))) && !values.is_empty() {
            self.step()?;
            let mut total = match values.first() {
                Some(Value::Float(value)) => f64::from_bits(*value),
                _ => unreachable!("non-empty all-float list has a first Float"),
            };
            for value in &values[1..] {
                self.step()?;
                let Value::Float(value) = value else {
                    unreachable!("all-float list was checked above")
                };
                total += f64::from_bits(*value);
            }
            return Ok(Value::Float(if total.is_nan() {
                CANONICAL_NAN_BITS
            } else {
                total.to_bits()
            }));
        }
        if values
            .iter()
            .all(|value| matches!(value, Value::Decimal(_)))
            && !values.is_empty()
        {
            self.step()?;
            let Value::Decimal(first) = &values[0] else {
                unreachable!("non-empty all-Decimal list has a first Decimal")
            };
            let mut total = first.clone();
            for value in &values[1..] {
                self.step()?;
                let Value::Decimal(value) = value else {
                    unreachable!("all-Decimal list was checked above")
                };
                total = total.add(value)?;
            }
            return Ok(Value::Decimal(total));
        }
        if !values.is_empty()
            && values
                .iter()
                .all(|value| matches!(value, Value::Money { .. }))
        {
            self.step()?;
            let Value::Money {
                amount: first_amount,
                currency,
            } = &values[0]
            else {
                unreachable!("non-empty all-Money list has a first Money");
            };
            let mut total = first_amount.clone();
            for value in &values[1..] {
                self.step()?;
                let Value::Money {
                    amount,
                    currency: other_currency,
                } = value
                else {
                    unreachable!("all-Money list was checked above");
                };
                if currency != other_currency {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
                total = self.checked_decimal(total.add(amount)?)?;
            }
            return Ok(Value::Money {
                amount: total,
                currency: *currency,
            });
        }
        let mut total = BigInt::ZERO;
        for value in values {
            self.step()?;
            let Value::Int(value) = value else {
                // Mixed numeric kinds, Money and affine quantities remain
                // outside this evaluator slice until their runtime contracts
                // exist.
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            };
            total = self.integer(&total + value)?;
        }
        Ok(Value::Int(total))
    }
    fn first(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        Ok(values
            .first()
            .cloned()
            .map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))))
    }
    fn last(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        Ok(Value::Option(values.last().cloned().map(Box::new)))
    }
    fn one(
        &mut self,
        values: &[Value],
        predicate: Option<&Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let Some(predicate) = predicate else {
            return match values {
                [] => Err(error("ORNA-EVAL-RELATION-ONE-ZERO")),
                [value] => Ok(value.clone()),
                _ => Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE")),
            };
        };

        let mut matching = None;
        let mut multiple = false;
        for value in values {
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    if matching.is_some() {
                        multiple = true;
                    } else {
                        matching = Some(value.clone());
                    }
                }
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        if multiple {
            return Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
        }
        matching.ok_or_else(|| error("ORNA-EVAL-RELATION-ONE-ZERO"))
    }
    fn every(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        for value in values {
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {}
                Value::Bool(false) => return Ok(Value::Bool(false)),
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Bool(true))
    }
    fn exists(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        for value in values {
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => return Ok(Value::Bool(true)),
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Bool(false))
    }
    fn take(&mut self, values: &[Value], count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let end = if count >= &BigInt::from(values.len()) {
            values.len()
        } else {
            count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?
        };
        self.items(end)?;
        let mut result = Vec::with_capacity(end);
        for value in &values[..end] {
            self.step()?;
            result.push(value.clone());
        }
        Ok(Value::list(result))
    }
    fn drop(&mut self, values: &[Value], count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let start = if count >= &BigInt::from(values.len()) {
            values.len()
        } else {
            count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?
        };
        let result_len = values.len() - start;
        self.items(result_len)?;
        let mut result = Vec::with_capacity(result_len);
        for value in &values[start..] {
            self.step()?;
            result.push(value.clone());
        }
        Ok(Value::list(result))
    }
    fn filter(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut filtered = Vec::new();
        for value in values {
            // Invoke once, in input order, and retain only an explicit true.
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    filtered.push(value.clone());
                    self.items(filtered.len())?;
                }
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::list(filtered))
    }
    fn group_by(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut groups = Vec::<(Value, Vec<Value>)>::new();
        for value in values {
            // Classify once, in input order. The returned key must have an
            // explicit total comparison; canonical encoding alone is not a
            // substitute for the library's lawful-key requirement.
            self.step()?;
            let group_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_group_key(&group_key)?;
            let mut matched = false;
            for (existing_key, rows) in &mut groups {
                if compare_group_keys(existing_key, &group_key)? == std::cmp::Ordering::Equal {
                    rows.push(value.clone());
                    self.items(rows.len())?;
                    matched = true;
                    break;
                }
            }
            if !matched {
                self.items(groups.len() + 1)?;
                groups.push((group_key, vec![value.clone()]));
            }
        }

        // Insertion sort keeps the ordering decision fallible and avoids
        // accepting an incidental host ordering for source values.
        for index in 1..groups.len() {
            let mut current = index;
            while current > 0
                && compare_group_keys(&groups[current - 1].0, &groups[current].0)?
                    == std::cmp::Ordering::Greater
            {
                groups.swap(current - 1, current);
                current -= 1;
            }
        }
        self.items(groups.len())?;
        Ok(Value::List(
            groups
                .into_iter()
                .map(|(key, rows)| Value::Tuple(vec![key, Value::list(rows)]))
                .collect(),
        ))
    }
    fn asof_join(
        &mut self,
        left: &[Value],
        right: &[Value],
        time: &Value,
        by: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        // Selectors are applied to finite rows, and results retain left input
        // order as `(left, right-or-null)` tuples. For each row, only right
        // timestamps at or before the left timestamp are eligible. The latest
        // eligible timestamp wins; canonical row bytes break equal-time ties.
        self.items(left.len())?;
        self.items(right.len())?;
        if left.is_empty() {
            return Ok(Value::list(Vec::new()));
        }

        let mut left_keys = Vec::with_capacity(left.len());
        for row in left {
            self.step()?;
            let time_key = self.invoke_predicate(time, row.clone(), depth + 1)?;
            lawful_asof_time(&time_key)?;
            self.step()?;
            let group_key = self.invoke_predicate(by, row.clone(), depth + 1)?;
            lawful_group_key(&group_key)?;
            left_keys.push((time_key, group_key));
            self.items(left_keys.len())?;
        }

        let mut right_keys = Vec::with_capacity(right.len());
        for row in right {
            self.step()?;
            let time_key = self.invoke_predicate(time, row.clone(), depth + 1)?;
            lawful_asof_time(&time_key)?;
            self.step()?;
            let group_key = self.invoke_predicate(by, row.clone(), depth + 1)?;
            lawful_group_key(&group_key)?;
            let canonical_key = row
                .clone()
                .canonical()?
                .encode()
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
            right_keys.push((time_key, group_key, canonical_key));
            self.items(right_keys.len())?;
        }

        let mut joined = Vec::with_capacity(left.len());
        for (left_row, (left_time, left_group)) in left.iter().zip(left_keys) {
            let mut selected = None::<(Value, Vec<u8>, Value)>;
            for (right_row, (right_time, right_group, canonical_key)) in
                right.iter().zip(&right_keys)
            {
                self.step()?;
                if compare_group_keys(&left_group, right_group)? != std::cmp::Ordering::Equal {
                    continue;
                }
                if compare_values(right_time, &left_time)? == std::cmp::Ordering::Greater {
                    continue;
                }
                let replace = match &selected {
                    None => true,
                    Some((best_time, best_key, _)) => {
                        match compare_values(right_time, best_time)? {
                            std::cmp::Ordering::Greater => true,
                            std::cmp::Ordering::Equal => canonical_key > best_key,
                            std::cmp::Ordering::Less => false,
                        }
                    }
                };
                if replace {
                    selected = Some((right_time.clone(), canonical_key.clone(), right_row.clone()));
                }
            }
            joined.push(Value::Tuple(vec![
                left_row.clone(),
                selected.map_or(Value::Null, |(_, _, row)| {
                    Value::Option(Some(Box::new(row)))
                }),
            ]));
            self.items(joined.len())?;
        }
        Ok(Value::list(joined))
    }
    pub(crate) fn invoke_callable(
        &mut self,
        callable: &Value,
        arguments: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let functions = self.functions;
        let (parameters, body, mut captured, session_owned, namespace) = match callable {
            Value::Function {
                name,
                nominal_definitions,
            } => {
                let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
                (
                    &function.parameters,
                    &function.body,
                    Scope::from_environment_with_nominals(
                        &function.environment,
                        nominal_definitions,
                        self,
                    )?,
                    self.session_functions
                        .is_some_and(|functions| functions.contains(name)),
                    function_namespace(name),
                )
            }
            Value::Closure(closure) => (
                &closure.parameters,
                &closure.body,
                closure.captured.clone(),
                self.repl_bindings,
                closure.namespace.clone(),
            ),
            _ => return Err(error("ORNA-EVAL-TYPE")),
        };
        self.items(arguments.len())?;
        if arguments.len() > parameters.len() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
        captured.2.extend(
            self.session_functions
                .into_iter()
                .flat_map(|functions| functions.iter().cloned())
                .filter(|name| captured.0.contains_key(name)),
        );
        let mut supplied = BTreeMap::new();
        for (index, value) in arguments.into_iter().enumerate() {
            supplied.insert(parameter_key(&parameters[index], index), value);
        }
        let previous_repl_bindings = self.repl_bindings;
        self.repl_bindings = session_owned;
        let previous_namespace = self.namespace.clone();
        self.namespace = namespace;
        let result = invoke_pure(self, parameters, body, captured, supplied, depth + 1);
        self.namespace = previous_namespace;
        self.repl_bindings = previous_repl_bindings;
        result
    }
    pub(crate) fn invoke_predicate(
        &mut self,
        callable: &Value,
        input: Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let functions = self.functions;
        let (parameters, body, captured, session_owned, namespace) = match callable {
            Value::Function {
                name,
                nominal_definitions,
            } => {
                let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
                (
                    &function.parameters,
                    &function.body,
                    Scope::from_environment_with_nominals(
                        &function.environment,
                        nominal_definitions,
                        self,
                    )?,
                    self.session_functions
                        .is_some_and(|functions| functions.contains(name)),
                    function_namespace(name),
                )
            }
            Value::Closure(closure) => (
                &closure.parameters,
                &closure.body,
                closure.captured.clone(),
                self.repl_bindings,
                closure.namespace.clone(),
            ),
            _ => return Err(error("ORNA-EVAL-TYPE")),
        };
        self.items(1)?;
        let Some(parameter) = parameters.first() else {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        };
        let mut supplied = BTreeMap::new();
        supplied.insert(parameter_key(parameter, 0), input);
        let previous_repl_bindings = self.repl_bindings;
        self.repl_bindings = session_owned;
        let previous_namespace = self.namespace.clone();
        self.namespace = namespace;
        let result = invoke_pure(self, parameters, body, captured, supplied, depth + 1);
        self.namespace = previous_namespace;
        self.repl_bindings = previous_repl_bindings;
        result
    }
    pub(crate) fn positive_collection_size(
        &self,
        value: &BigInt,
    ) -> Result<usize, EvaluationError> {
        if !value.is_positive() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(value.to_usize().unwrap_or(usize::MAX))
    }
    fn strictly_ordered_time_series(&mut self, points: &[Value]) -> Result<Value, EvaluationError> {
        self.items(points.len())?;
        let mut previous_time: Option<&Value> = None;
        for point in points {
            self.step()?;
            let Value::Tuple(pair) = point else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [time, _] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if !matches!(time, Value::Instant { .. }) {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            if let Some(previous_time) = previous_time
                && compare_values(previous_time, time)?.is_ge()
            {
                return Ok(Value::Bool(false));
            }
            previous_time = Some(time);
        }
        Ok(Value::Bool(true))
    }
    fn window_time_series_statistics(
        &mut self,
        points: &[Value],
        size: &BigInt,
        step: &BigInt,
        statistic: &str,
    ) -> Result<Value, EvaluationError> {
        // Visit complete windows in source order and discard each temporary
        // window after computing its statistic. This keeps sparse timestamp
        // order attached to each result without retaining every overlapping
        // window at once.
        self.items(points.len())?;
        let size = self.positive_collection_size(size)?;
        let step = self.positive_collection_size(step)?;
        if size > points.len() {
            return Ok(Value::list(Vec::new()));
        }
        for _ in points {
            self.step()?;
        }

        let last_start = points.len() - size;
        let mut start = 0usize;
        let mut results = Vec::new();
        while start <= last_start {
            let end = start + size;
            self.items(size)?;
            self.step()?;
            let window = points[start..end].to_vec();
            self.step()?;
            let result = if statistic == "rate_integrate" {
                let rate = self.stats_rate(&window)?;
                let integral = self.stats_integrate(&window)?;
                Value::Tuple(vec![
                    Value::Option(Some(Box::new(rate))),
                    Value::Option(Some(Box::new(integral))),
                ])
            } else {
                let result = self.stats(statistic, vec![Value::list(window)])?;
                if matches!(statistic, "rate" | "integrate") {
                    Value::Option(Some(Box::new(result)))
                } else {
                    result
                }
            };
            results.push(result);
            self.items(results.len())?;

            let Some(next) = start.checked_add(step) else {
                break;
            };
            start = next;
        }
        Ok(Value::list(results))
    }
    pub(crate) fn list_stream_digest(
        &mut self,
        source_label: &str,
        values: &[Value],
    ) -> Result<[u8; 32], EvaluationError> {
        self.string(source_label.to_owned())?;
        self.items(values.len())?;
        let mut encoded_values = Vec::with_capacity(values.len());
        for value in values {
            self.step()?;
            encoded_values.push(value.raw()?);
            self.items(encoded_values.len())?;
        }
        let identity = Raw::Array(vec![
            Raw::Text(source_label.to_owned()),
            Raw::Array(encoded_values),
        ]);
        domain_digest("orna.list.v1", &identity).map_err(|_| error("ORNA-EVAL-VALUE"))
    }
}
