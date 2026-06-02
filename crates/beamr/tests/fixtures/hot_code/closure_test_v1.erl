-module(closure_test).
-export([make/0, call/1, version/0]).

version() -> 1.
make() -> fun version/0.
call(F) -> F().
