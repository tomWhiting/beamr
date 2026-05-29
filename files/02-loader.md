# 02 · The Loader — Reading Compiled Gleam

## What it is

The loader takes a `.beam` file off disk — the thing the Gleam toolchain produces —
and turns it into something the machine can actually run. It's the front door.

A quick reality check that shapes everything: Gleam doesn't emit `.beam` directly.
Gleam compiles to Erlang *source*, and the Erlang compiler (`erlc`) turns that into
`.beam`. So `erlc` sits in your build pipeline. The upside is that the `.beam`
format is a more stable target than Gleam's intermediate output — it changes slowly,
on Erlang's release cadence, not Gleam's whims.

## Why it has to exist

Because the unit of code we run is the compiled module, and it arrives as a chunked
binary file — a little container format with labelled sections: one chunk for the
atom table, one for the actual instructions, one for string literals, one for the
*imports* (functions this module calls but doesn't define), one for the *exports*
(functions it offers to others), one for embedded constant data. The loader's job is
to open that container, read each chunk, and assemble a runnable module: decode the
raw instruction bytes into something structured, hook the atom numbers up to the
shared atom table, and resolve the imports.

## The intuition

It's an unpacker and a wiring loom. Unpack the box; connect every loose wire (this
module wants `lists:map` — where does that live?) to its socket. A module that comes
out the far end has no dangling references: every name it mentions points at
something real, or the loader refuses it.

## What's quietly tricky — and a gift hidden in it

Decoding the instruction stream faithfully is finicky; the encoding has compact
forms and tagged operands, and small mistakes produce confident nonsense. The
`beam_file` crate already does the container-parsing part and is fine to lean on as
a reference.

But here's the gift: **the import chunk is your to-do list.** Every external function
a module calls is listed there. When the loader tries to resolve imports and finds
one it doesn't have — say some `erlang:` built-in we haven't written yet — that's
not a failure, it's a *signal*: "this workflow needs this built-in; go build it."
The unresolved-import report becomes a demand-driven work queue that physically
**cannot grow to "all of Erlang" unless your workflows do.** That's the mechanism
that keeps the whole project scoped and finishable. The import table is the leash.

## How it connects

Fills the **Atom** table. Produces modules the **Interpreter** runs. Its
unresolved-import report defines exactly which **Native/BIF** functions are worth
writing next, and no more.
