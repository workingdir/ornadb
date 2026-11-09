//! Standard-library dispatch for the stats family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    fn stats_options(&self, values: &[Value]) -> Result<Option<(usize, bool)>, EvaluationError> {
        if values.is_empty() {
            return Ok(None);
        }
        if matches!(values, [Value::Null, Value::Null]) {
            return Ok(None);
        }
        if values.len() != 2 {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let Value::Int(scale) = &values[0] else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if scale.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let scale = scale.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if scale > self.limits.max_integer_digits {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let Value::String(rounding) = &values[1] else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if rounding != "half_even" {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(Some((scale, true)))
    }
    fn exact_decimal_result(
        &self,
        value: DecimalValue,
        preserve_decimal: bool,
    ) -> Result<Value, EvaluationError> {
        let value = self.checked_decimal(value)?;
        if preserve_decimal || value.exponent10.is_negative() {
            return Ok(Value::Decimal(value));
        }
        let power = value
            .exponent10
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let integer = self.integer(value.coefficient * BigInt::from(10u8).pow(power as u32))?;
        Ok(Value::Int(integer))
    }
    fn divide_stats(
        &self,
        left: &DecimalValue,
        right: &DecimalValue,
        options: Option<(usize, bool)>,
        preserve_decimal: bool,
    ) -> Result<Value, EvaluationError> {
        match left.divide(right) {
            Ok(value) => {
                let value = if let Some((scale, _)) = options {
                    value.round_to_scale(scale)?
                } else {
                    value
                };
                self.exact_decimal_result(value, preserve_decimal)
            }
            Err(failure) if matches!(failure.code(), "InexactDivision" | "ORNA-EVAL-VALUE") => {
                let Some((scale, _)) = options else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                self.exact_decimal_result(left.divide_rounded(right, scale)?, preserve_decimal)
            }
            Err(failure) => Err(failure),
        }
    }
    fn numeric_decimal(&self, value: &Value) -> Result<(DecimalValue, bool), EvaluationError> {
        match value {
            Value::Int(value) => Ok((DecimalValue::new(value.clone(), BigInt::zero())?, false)),
            Value::Decimal(value) => Ok((value.clone(), true)),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn variance(
        &mut self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let Some(first) = values.first() else {
            return Ok(Value::Null);
        };
        if values.iter().all(|value| matches!(value, Value::Float(_))) {
            let mut sum = 0.0;
            for value in values {
                self.step()?;
                let Value::Float(bits) = value else {
                    unreachable!()
                };
                sum += f64::from_bits(*bits);
            }
            let mean = sum / values.len() as f64;
            let mut squares = 0.0;
            for value in values {
                self.step()?;
                let Value::Float(bits) = value else {
                    unreachable!()
                };
                let delta = f64::from_bits(*bits) - mean;
                squares += delta * delta;
            }
            return Ok(float_value(squares / values.len() as f64));
        }
        if !matches!(first, Value::Int(_) | Value::Decimal(_))
            || !values
                .iter()
                .all(|value| matches!(value, Value::Int(_) | Value::Decimal(_)))
        {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        let preserve_decimal = values
            .iter()
            .any(|value| matches!(value, Value::Decimal(_)));
        let zero = DecimalValue::new(BigInt::zero(), BigInt::zero())?;
        let mut sum = zero.clone();
        let mut squares = zero;
        for value in values {
            self.step()?;
            let (value, _) = self.numeric_decimal(value)?;
            sum = sum.add(&value)?;
            squares = squares.add(&value.multiply(&value)?)?;
        }
        let count = DecimalValue::new(BigInt::from(values.len()), BigInt::zero())?;
        let numerator = squares.multiply(&count)?.add(
            &DecimalValue::new(-sum.coefficient.clone(), sum.exponent10.clone())?.multiply(&sum)?,
        )?;
        let denominator = DecimalValue::new(
            BigInt::from(values.len()) * BigInt::from(values.len()),
            BigInt::zero(),
        )?;
        self.divide_stats(&numerator, &denominator, options, preserve_decimal)
    }
    fn square_root(
        &self,
        value: &DecimalValue,
        options: Option<(usize, bool)>,
        preserve_decimal: bool,
    ) -> Result<Value, EvaluationError> {
        if value.coefficient.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let mut coefficient = value.coefficient.clone();
        let mut exponent = value.exponent10.clone();
        if exponent.is_odd() {
            coefficient *= 10;
            exponent -= 1;
        }
        let root = coefficient.sqrt();
        if &root * &root == coefficient {
            let result = DecimalValue::new(root, exponent / 2)?;
            return self.exact_decimal_result(result, preserve_decimal);
        }
        let Some((scale, _)) = options else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let scale_i32 = i32::try_from(scale).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
        let shift = value.exponent10.clone() + BigInt::from(scale_i32 * 2);
        let (numerator, denominator) = if shift.is_negative() {
            let power = (-shift)
                .to_u32()
                .filter(|power| *power <= self.limits.max_integer_digits as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            (value.coefficient.clone(), BigInt::from(10u8).pow(power))
        } else {
            let power = shift
                .to_u32()
                .filter(|power| *power <= self.limits.max_integer_digits as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            (
                &value.coefficient * BigInt::from(10u8).pow(power),
                BigInt::from(1u8),
            )
        };
        let floor = (&numerator / &denominator).sqrt();
        let exact = &floor * &floor * &denominator == numerator;
        let rounded = if exact {
            floor
        } else {
            let midpoint_twice = &floor * 2 + 1;
            let lhs = &numerator * 4;
            let rhs = &denominator * &midpoint_twice * &midpoint_twice;
            if lhs > rhs || (lhs == rhs && floor.is_odd()) {
                floor + 1
            } else {
                floor
            }
        };
        self.exact_decimal_result(
            DecimalValue::new(rounded, -BigInt::from(scale_i32))?,
            preserve_decimal,
        )
    }
    fn standard_deviation(
        &mut self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        let variance = self.variance(values, options)?;
        match variance {
            Value::Null => Ok(Value::Null),
            Value::Float(bits) => Ok(float_value(f64::from_bits(bits).sqrt())),
            Value::Int(_) | Value::Decimal(_) => {
                let (variance, preserve_decimal) = self.numeric_decimal(&variance)?;
                self.square_root(&variance, options, preserve_decimal)
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn stats_range(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        let low = self.extreme("min", values)?;
        let high = self.extreme("max", values)?;
        let (Value::Option(Some(low)), Value::Option(Some(high))) = (low, high) else {
            return Ok(Value::Null);
        };
        self.apply_binary("-", *high, *low)
    }
    fn stats_mode(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut counts: Vec<(Value, usize)> = Vec::new();
        for value in values {
            self.step()?;
            if let Some((_, count)) = counts.iter_mut().find(|(current, _)| {
                matches!(
                    self.apply_binary("==", current.clone(), value.clone()),
                    Ok(Value::Bool(true))
                )
            }) {
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            } else {
                counts.push((value.clone(), 1));
                self.items(counts.len())?;
            }
        }
        let max_count = counts.iter().map(|(_, count)| *count).max().unwrap_or(0);
        Ok(Value::List(
            counts
                .into_iter()
                .filter_map(|(value, count)| (count == max_count && max_count > 0).then_some(value))
                .collect(),
        ))
    }
    fn stats_histogram(
        &mut self,
        values: &[Value],
        bins: &[Value],
        include_final_upper: bool,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        self.items(bins.len())?;
        let mut bounds = Vec::with_capacity(bins.len());
        let mut previous_upper: Option<&Value> = None;
        for bin in bins {
            let Value::Tuple(pair) = bin else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [lower, upper] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let overlaps_previous = if let Some(previous) = previous_upper {
                compare_sort_keys(previous, lower)?.is_gt()
            } else {
                false
            };
            if compare_sort_keys(lower, upper)?.is_ge() || overlaps_previous {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            bounds.push((lower.clone(), upper.clone()));
            previous_upper = Some(upper);
        }
        let mut counts = vec![0usize; bounds.len()];
        for value in values {
            self.step()?;
            for (index, (lower, upper)) in bounds.iter().enumerate() {
                let lower_ok = compare_sort_keys(value, lower)?.is_ge();
                let upper_order = compare_sort_keys(value, upper)?;
                let final_inclusive = include_final_upper && index + 1 == bounds.len();
                if lower_ok && (upper_order.is_lt() || (final_inclusive && upper_order.is_eq())) {
                    counts[index] = counts[index]
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                    break;
                }
            }
        }
        Ok(Value::List(
            counts
                .into_iter()
                .map(|count| Value::Int(BigInt::from(count)))
                .collect(),
        ))
    }
    fn elapsed_seconds(&self, start: &Value, end: &Value) -> Result<DecimalValue, EvaluationError> {
        let (
            Value::Instant {
                unix_seconds: start_seconds,
                nanosecond: start_nanos,
            },
            Value::Instant {
                unix_seconds: end_seconds,
                nanosecond: end_nanos,
            },
        ) = (start, end)
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let nanoseconds = elapsed_total_nanoseconds(&BigInt::from(*end_seconds), *end_nanos)
            - elapsed_total_nanoseconds(&BigInt::from(*start_seconds), *start_nanos);
        if nanoseconds <= BigInt::zero() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        DecimalValue::new(nanoseconds, BigInt::from(-9))
    }
    fn series_points<'a>(
        &self,
        points: &'a [Value],
    ) -> Result<Vec<(&'a Value, &'a Value)>, EvaluationError> {
        let mut result: Vec<(&Value, &Value)> = Vec::with_capacity(points.len());
        for point in points {
            let Value::Tuple(pair) = point else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [time, value] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if !matches!(time, Value::Instant { .. }) {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            if let Some((previous, _)) = result.last()
                && compare_values(previous, time)?.is_ge()
            {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            result.push((time, value));
        }
        if result.len() < 2 {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(result)
    }
    fn series_slope(
        &self,
        start_time: &Value,
        start_value: &Value,
        end_time: &Value,
        end_value: &Value,
    ) -> Result<Value, EvaluationError> {
        let elapsed = self.elapsed_seconds(start_time, end_time)?;
        match (start_value, end_value) {
            (Value::Float(start), Value::Float(end)) => Ok(float_value(
                (f64::from_bits(*end) - f64::from_bits(*start)) / decimal_to_f64(&elapsed)?,
            )),
            (Value::Int(_) | Value::Decimal(_), Value::Int(_) | Value::Decimal(_)) => {
                let (start, start_decimal) = self.numeric_decimal(start_value)?;
                let (end, end_decimal) = self.numeric_decimal(end_value)?;
                let delta = end.add(&DecimalValue::new(-start.coefficient, start.exponent10)?)?;
                self.divide_stats(&delta, &elapsed, None, start_decimal || end_decimal)
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    pub(crate) fn stats_rate(&self, points: &[Value]) -> Result<Value, EvaluationError> {
        let points = self.series_points(points)?;
        let (start_time, start_value) = points[0];
        let (end_time, end_value) = points[points.len() - 1];
        self.series_slope(start_time, start_value, end_time, end_value)
    }
    fn stats_derivative(&mut self, points: &[Value]) -> Result<Value, EvaluationError> {
        let points = self.series_points(points)?;
        let mut derivatives = Vec::with_capacity(points.len() - 1);
        for pair in points.windows(2) {
            self.step()?;
            derivatives.push(Value::Tuple(vec![
                pair[1].0.clone(),
                self.series_slope(pair[0].0, pair[0].1, pair[1].0, pair[1].1)?,
            ]));
            self.items(derivatives.len())?;
        }
        Ok(Value::list(derivatives))
    }
    pub(crate) fn stats_integrate(&self, points: &[Value]) -> Result<Value, EvaluationError> {
        let points = self.series_points(points)?;
        if points
            .iter()
            .all(|(_, value)| matches!(value, Value::Float(_)))
        {
            let mut total = 0.0;
            for pair in points.windows(2) {
                let (Value::Float(left), Value::Float(right)) = (pair[0].1, pair[1].1) else {
                    unreachable!()
                };
                let elapsed = decimal_to_f64(&self.elapsed_seconds(pair[0].0, pair[1].0)?)?;
                total += (f64::from_bits(*left) + f64::from_bits(*right)) * 0.5 * elapsed;
            }
            return Ok(float_value(total));
        }
        let mut total = DecimalValue::new(BigInt::zero(), BigInt::zero())?;
        let mut preserve_decimal = false;
        for pair in points.windows(2) {
            let (left, left_decimal) = self.numeric_decimal(pair[0].1)?;
            let (right, right_decimal) = self.numeric_decimal(pair[1].1)?;
            preserve_decimal |= left_decimal || right_decimal;
            let sum = left.add(&right)?;
            let average = self.divide_stats(
                &sum,
                &DecimalValue::new(BigInt::from(2), BigInt::zero())?,
                None,
                preserve_decimal,
            )?;
            let (average, average_decimal) = self.numeric_decimal(&average)?;
            preserve_decimal |= average_decimal;
            let area = average.multiply(&self.elapsed_seconds(pair[0].0, pair[1].0)?)?;
            total = total.add(&area)?;
        }
        self.exact_decimal_result(total, preserve_decimal)
    }
    pub(crate) fn stats(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        let name = name.strip_prefix("__").unwrap_or(name);
        match name {
            "mean" | "median" | "variance" | "standard_deviation" => {
                let [Value::List(rows), rest @ ..] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                let options = self.stats_options(rest)?;
                self.items(rows.len())?;
                if rows.is_empty() {
                    return Ok(Value::Null);
                }
                match name {
                    "mean" => self.mean(rows, options),
                    "median" => self.median(rows, options),
                    "variance" => self.variance(rows, options),
                    "standard_deviation" => self.standard_deviation(rows, options),
                    _ => unreachable!(),
                }
            }
            "percentile" => {
                let [
                    Value::List(rows),
                    probability,
                    Value::String(interpolation),
                    rest @ ..,
                ] = values.as_slice()
                else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.percentile(rows, probability, interpolation, self.stats_options(rest)?)
            }
            "sum" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.sum(rows)
            }
            "min" | "max" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.extreme(name, rows)
            }
            "range" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.stats_range(rows)
            }
            "mode" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.stats_mode(rows)
            }
            "histogram" => {
                let [Value::List(rows), Value::List(bins), rest @ ..] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                let include_final_upper = match rest {
                    [] => false,
                    [Value::Bool(flag)] => *flag,
                    _ => return Err(error("ORNA-EVAL-TYPE")),
                };
                self.stats_histogram(rows, bins, include_final_upper)
            }
            "rate" | "derivative" | "integrate" => {
                let [Value::List(points)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                match name {
                    "rate" => self.stats_rate(points),
                    "derivative" => self.stats_derivative(points),
                    "integrate" => self.stats_integrate(points),
                    _ => unreachable!(),
                }
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn mean(
        &self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        let count = DecimalValue::new(BigInt::from(values.len()), BigInt::zero())?;
        match values.first() {
            Some(Value::Int(_)) if values.iter().all(|value| matches!(value, Value::Int(_))) => {
                let mut total = BigInt::zero();
                for value in values {
                    let Value::Int(value) = value else {
                        unreachable!()
                    };
                    total = self.integer(total + value)?;
                }
                self.divide_stats(
                    &DecimalValue::new(total, BigInt::zero())?,
                    &count,
                    options,
                    false,
                )
            }
            Some(Value::Decimal(_))
                if values
                    .iter()
                    .all(|value| matches!(value, Value::Decimal(_))) =>
            {
                let Value::Decimal(first) = &values[0] else {
                    unreachable!()
                };
                let mut total = first.clone();
                for value in &values[1..] {
                    let Value::Decimal(value) = value else {
                        unreachable!()
                    };
                    total = total.add(value)?;
                }
                self.divide_stats(&total, &count, options, true)
            }
            Some(Value::Float(_))
                if values.iter().all(|value| matches!(value, Value::Float(_))) =>
            {
                let Value::Float(first) = values.first().expect("non-empty Float mean") else {
                    unreachable!("Float branch requires a Float first value")
                };
                let mut total = f64::from_bits(*first);
                for value in &values[1..] {
                    let Value::Float(bits) = value else {
                        unreachable!()
                    };
                    total += f64::from_bits(*bits);
                }
                Ok(float_value(total / values.len() as f64))
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn median(
        &mut self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        let Some(first) = values.first() else {
            return Ok(Value::Null);
        };
        let homogeneous = match first {
            Value::Int(_) => values.iter().all(|value| matches!(value, Value::Int(_))),
            Value::Decimal(_) => values
                .iter()
                .all(|value| matches!(value, Value::Decimal(_))),
            Value::Float(_) => values.iter().all(|value| matches!(value, Value::Float(_))),
            _ => false,
        };
        if !homogeneous {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let mut sorted = values.to_vec();
        for index in 1..sorted.len() {
            let mut current = index;
            while current > 0 {
                self.step()?;
                if compare_sort_keys(&sorted[current - 1], &sorted[current])?
                    != std::cmp::Ordering::Greater
                {
                    break;
                }
                sorted.swap(current - 1, current);
                current -= 1;
            }
        }
        if sorted.len() % 2 == 1 {
            return Ok(sorted[sorted.len() / 2].clone());
        }
        self.average_values(
            &sorted[sorted.len() / 2 - 1],
            &sorted[sorted.len() / 2],
            options,
        )
    }
    fn average_values(
        &self,
        left: &Value,
        right: &Value,
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        match (left, right) {
            (Value::Int(left), Value::Int(right)) => {
                let total = self.integer(left + right)?;
                self.divide_stats(
                    &DecimalValue::new(total, BigInt::zero())?,
                    &DecimalValue::new(2.into(), BigInt::zero())?,
                    options,
                    false,
                )
            }
            (Value::Decimal(left), Value::Decimal(right)) => {
                let total = left.add(right)?;
                self.divide_stats(
                    &total,
                    &DecimalValue::new(2.into(), BigInt::zero())?,
                    options,
                    true,
                )
            }
            (Value::Float(left), Value::Float(right)) => Ok(float_value(
                (f64::from_bits(*left) + f64::from_bits(*right)) / 2.0,
            )),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn percentile(
        &self,
        values: &[Value],
        probability: &Value,
        interpolation: &str,
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        if !matches!(
            interpolation,
            "linear" | "lower" | "higher" | "nearest" | "midpoint"
        ) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.items(values.len())?;
        let probability = match probability {
            Value::Int(value) if !value.is_negative() => {
                DecimalValue::new(value.clone(), BigInt::zero())?
            }
            Value::Decimal(value) => value.clone(),
            Value::Float(value) if f64::from_bits(*value).is_finite() => {
                if !((0.0..=1.0).contains(&f64::from_bits(*value))) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                return self.percentile_float(values, f64::from_bits(*value), interpolation);
            }
            _ => return Err(error("ORNA-EVAL-VALUE")),
        };
        let zero = DecimalValue::new(BigInt::zero(), BigInt::zero())?;
        let one = DecimalValue::new(BigInt::from(1u8), BigInt::zero())?;
        if compare_values(&Value::Decimal(probability.clone()), &Value::Decimal(zero))?
            == std::cmp::Ordering::Less
            || compare_values(&Value::Decimal(probability.clone()), &Value::Decimal(one))?
                == std::cmp::Ordering::Greater
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if values.is_empty() {
            return Ok(Value::Null);
        }
        let Some(first) = values.first() else {
            return Ok(Value::Null);
        };
        let homogeneous = match first {
            Value::Int(_) => values.iter().all(|value| matches!(value, Value::Int(_))),
            Value::Decimal(_) => values
                .iter()
                .all(|value| matches!(value, Value::Decimal(_))),
            Value::Float(_) => values.iter().all(|value| matches!(value, Value::Float(_))),
            _ => false,
        };
        if !homogeneous {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let mut sorted = values.to_vec();
        for index in 1..sorted.len() {
            let mut current = index;
            while current > 0
                && compare_sort_keys(&sorted[current - 1], &sorted[current])?
                    == std::cmp::Ordering::Greater
            {
                sorted.swap(current - 1, current);
                current -= 1;
            }
        }
        let values = sorted.as_slice();
        let span = DecimalValue::new(BigInt::from(values.len() - 1), BigInt::zero())?;
        let position = probability.multiply(&span)?;
        let (index, fraction) = decimal_floor_fraction(&position)?;
        let lower = values.get(index).ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let upper = values.get(index.saturating_add(1)).unwrap_or(lower);
        match interpolation {
            "lower" => Ok(lower.clone()),
            "higher" => {
                if fraction.coefficient.is_zero() {
                    Ok(lower.clone())
                } else {
                    Ok(upper.clone())
                }
            }
            "nearest" => {
                let half = DecimalValue::new(5.into(), (-1).into())?;
                if compare_values(&Value::Decimal(fraction), &Value::Decimal(half))?
                    == std::cmp::Ordering::Less
                {
                    Ok(lower.clone())
                } else {
                    Ok(upper.clone())
                }
            }
            "midpoint" if fraction.coefficient.is_zero() => Ok(lower.clone()),
            "midpoint" => self.average_values(lower, upper, options),
            "linear" if fraction.coefficient.is_zero() => Ok(lower.clone()),
            "linear" => self.interpolate_exact(lower, upper, &fraction, options),
            _ => Err(error("ORNA-EVAL-VALUE")),
        }
    }
    fn interpolate_exact(
        &self,
        lower: &Value,
        upper: &Value,
        fraction: &DecimalValue,
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        match (lower, upper) {
            (Value::Int(lower), Value::Int(upper)) => {
                let low = DecimalValue::new(lower.clone(), BigInt::zero())?;
                let high = DecimalValue::new(upper.clone(), BigInt::zero())?;
                let delta = high.add(&DecimalValue::new(-lower.clone(), BigInt::zero())?)?;
                let result = low.add(&delta.multiply(fraction)?)?;
                let result = if let Some((scale, _)) = options {
                    result.round_to_scale(scale)?
                } else {
                    result
                };
                self.exact_decimal_result(result, false)
            }
            (Value::Decimal(lower), Value::Decimal(upper)) => {
                let delta = upper.add(&DecimalValue::new(
                    -lower.coefficient.clone(),
                    lower.exponent10.clone(),
                )?)?;
                let result = lower.add(&delta.multiply(fraction)?)?;
                let result = if let Some((scale, _)) = options {
                    result.round_to_scale(scale)?
                } else {
                    result
                };
                self.exact_decimal_result(result, true)
            }
            (Value::Float(lower), Value::Float(upper)) => Ok(float_value(
                f64::from_bits(*lower)
                    + (f64::from_bits(*upper) - f64::from_bits(*lower)) * decimal_to_f64(fraction)?,
            )),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn percentile_float(
        &self,
        values: &[Value],
        probability: f64,
        interpolation: &str,
    ) -> Result<Value, EvaluationError> {
        if values.is_empty() {
            return Ok(Value::Null);
        }
        if !values.iter().all(|value| matches!(value, Value::Float(_))) {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let mut sorted = values
            .iter()
            .map(|value| match value {
                Value::Float(bits) => *bits,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        sorted.sort_by(|left, right| float_total_cmp(*left, *right));
        let position = probability * (sorted.len() - 1) as f64;
        let lower = position.floor() as usize;
        let upper = position.ceil() as usize;
        let fraction = position - lower as f64;
        let choose = match interpolation {
            "lower" => lower,
            "higher" => upper,
            "nearest" => {
                if fraction < 0.5 {
                    lower
                } else {
                    upper
                }
            }
            _ => lower,
        };
        if interpolation == "linear" {
            return Ok(float_value(
                f64::from_bits(sorted[lower])
                    + (f64::from_bits(sorted[upper]) - f64::from_bits(sorted[lower])) * fraction,
            ));
        }
        if interpolation == "midpoint" {
            return Ok(float_value(
                (f64::from_bits(sorted[lower]) + f64::from_bits(sorted[upper])) / 2.0,
            ));
        }
        Ok(Value::Float(sorted[choose]))
    }
    pub(crate) fn extreme(
        &mut self,
        name: &str,
        values: &[Value],
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        if values.iter().all(|value| matches!(value, Value::Float(_))) {
            for _ in values {
                self.step()?;
            }
            let bits = values
                .iter()
                .map(|value| match value {
                    Value::Float(bits) => *bits,
                    _ => unreachable!("all-float list was checked above"),
                })
                .collect::<Vec<_>>();
            let result = if name == "min" {
                float_min(&bits)
            } else {
                float_max(&bits)
            };
            return Ok(result.map_or(Value::Null, |value| {
                Value::Option(Some(Box::new(Value::Float(value))))
            }));
        }
        if !values.is_empty()
            && values
                .iter()
                .all(|value| matches!(value, Value::Decimal(_)))
        {
            let mut candidate = None;
            for value in values {
                self.step()?;
                let replace = match candidate.as_ref() {
                    None => true,
                    Some(current) => {
                        let ordering = compare_values(value, current)?;
                        if name == "min" {
                            ordering.is_lt()
                        } else {
                            ordering.is_gt()
                        }
                    }
                };
                if replace {
                    // Strict comparison retains the first equal Decimal,
                    // including values that differ only by representational scale.
                    candidate = Some(value.clone());
                }
            }
            return Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))));
        }
        if !values.is_empty()
            && values
                .iter()
                .all(|value| matches!(value, Value::Money { .. }))
        {
            return self.extreme_money(name, values);
        }
        let temporal_kind = values.first().and_then(range_endpoint_kind);
        if matches!(temporal_kind, Some("Date" | "Instant" | "Duration"))
            && values
                .iter()
                .all(|value| range_endpoint_kind(value) == temporal_kind)
        {
            let mut candidate = None;
            for value in values {
                self.step()?;
                let replace = match candidate.as_ref() {
                    None => true,
                    Some(current) => {
                        let ordering = compare_values(value, current)?;
                        if name == "min" {
                            ordering.is_lt()
                        } else {
                            ordering.is_gt()
                        }
                    }
                };
                if replace {
                    // Strict comparison retains the first equal temporal
                    // value, including equivalent Instant offsets.
                    candidate = Some(value.clone());
                }
            }
            return Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))));
        }
        let mut candidate: Option<Value> = None;
        for value in values {
            self.step()?;
            lawful_sort_key(value)?;
            let replace = match candidate.as_ref() {
                None => true,
                Some(current) => {
                    let ordering = compare_sort_keys(value, current)?;
                    if name == "min" {
                        ordering.is_lt()
                    } else {
                        ordering.is_gt()
                    }
                }
            };
            if replace {
                // Strict comparison above deliberately retains the first
                // equal candidate, preserving observable input order.
                candidate = Some(value.clone());
            }
        }
        Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))))
    }
    /// Money extremes require one shared currency; a mixed list is unsupported.
    fn extreme_money(&mut self, name: &str, values: &[Value]) -> Result<Value, EvaluationError> {
        let currency = match &values[0] {
            Value::Money { currency, .. } => *currency,
            _ => unreachable!("non-empty all-Money list has a first Money"),
        };
        let mut candidate = None;
        for value in values {
            self.step()?;
            let Value::Money {
                currency: value_currency,
                ..
            } = value
            else {
                unreachable!("all-Money list was checked above");
            };
            if *value_currency != currency {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            let replace = match candidate.as_ref() {
                None => true,
                Some(current) => {
                    let ordering = compare_values(value, current)?;
                    if name == "min" {
                        ordering.is_lt()
                    } else {
                        ordering.is_gt()
                    }
                }
            };
            if replace {
                candidate = Some(value.clone());
            }
        }
        Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))))
    }
}
