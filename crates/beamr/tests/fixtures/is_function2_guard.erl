-module(is_function2_guard).
-export([check/1, matching_arity/0, wrong_arity/0, not_a_fun/0]).

%% A literal arity makes the compiler emit the is_function2 test opcode
%% (a variable arity compiles to the erlang:is_function/2 guard BIF
%% instead).
check(F) when is_function(F, 2) -> matched;
check(_) -> fallthrough.

matching_arity() -> check(fun(A, B) -> {A, B} end).

wrong_arity() -> check(fun(A) -> A end).

not_a_fun() -> check(nope).
