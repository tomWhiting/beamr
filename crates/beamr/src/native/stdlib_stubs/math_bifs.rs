//! Math module BIFs.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::boxed::Float;

pub fn bif_ceil(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    make_float(context, number_to_f64(*value)?.ceil())
}

pub fn bif_floor(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    make_float(context, number_to_f64(*value)?.floor())
}

pub fn bif_exp(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    make_float(context, number_to_f64(*value)?.exp())
}

pub fn bif_log(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };
    let value = number_to_f64(*value)?;
    if value <= 0.0 {
        return Err(badarg());
    }
    make_float(context, value.ln())
}

pub fn bif_pow(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [base, exponent] = args else {
        return Err(badarg());
    };
    make_float(
        context,
        number_to_f64(*base)?.powf(number_to_f64(*exponent)?),
    )
}

fn number_to_f64(term: Term) -> Result<f64, Term> {
    if let Some(value) = term.as_small_int() {
        return Ok(value as f64);
    }

    let value = Float::new(term).ok_or_else(badarg)?.value();
    if value.is_finite() {
        Ok(value)
    } else {
        Err(badarg())
    }
}

fn make_float(context: &mut ProcessContext, value: f64) -> Result<Term, Term> {
    if !value.is_finite() {
        return Err(badarg());
    }
    context.alloc_float(value)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}
