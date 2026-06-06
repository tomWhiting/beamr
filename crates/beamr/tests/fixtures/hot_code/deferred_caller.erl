-module(deferred_caller).
-export([call_counter/0]).

call_counter() -> counter:version().
