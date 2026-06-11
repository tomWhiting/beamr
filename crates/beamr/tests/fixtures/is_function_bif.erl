%% Fixture exercising erlang:is_function/1 and is_function/2 as callable
%% BIFs rather than the literal-arity is_function2 test opcode: the
%% compiler emits the guard-BIF instruction (`bif` with an
%% erlang:is_function import) for body-position calls and for guards whose
%% arity is not a compile-time literal. Compile with
%% `erlc is_function_bif.erl` (OTP 25+) and commit the .beam next to this
%% source.
-module(is_function_bif).
-export([body_one/0, body_two/0, guard_variable_arity/0, guard_badarg/0,
         arity_badarg/0, id/1, check/2]).

%% Exported so the compiler cannot constant-fold the type tests or infer
%% argument types at the call sites (type inference would otherwise turn
%% the variable-arity guard back into the is_function2 test opcode).
id(X) -> X.

%% Body-position is_function/1 on a fun and a non-fun.
body_one() ->
    {is_function(id(fun() -> ok end)), is_function(id(nope))}.

%% Body-position is_function/2: exact arity matches, others do not.
body_two() ->
    F = id(fun(A, B) -> {A, B} end),
    {is_function(F, 2), is_function(F, 3), is_function(id(nope), 2)}.

%% A variable arity compiles to the erlang:is_function/2 guard BIF.
guard_variable_arity() ->
    F = id(fun(A) -> A end),
    {check(F, 1), check(F, 2), check(nope, 1)}.

check(F, N) when is_function(F, N) -> matched;
check(_, _) -> fallthrough.

%% An invalid arity (negative or non-integer) in GUARD position must fail
%% the guard and take the false branch, not crash the process.
guard_badarg() ->
    F = id(fun(A) -> A end),
    {check(F, -1), check(F, not_an_arity)}.

%% A negative arity in body position raises badarg.
arity_badarg() ->
    is_function(id(fun() -> ok end), id(-1)).
