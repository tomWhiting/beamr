%% Fixture exercising caught-exception exit paths. Compile with
%% `erlc caught_exception_exit.erl` (OTP 25+) and commit the .beam next
%% to this source.
-module(caught_exception_exit).
-export([catch_then_normal/0, rethrow_unmatched_class/0]).

%% Catches an exception, handles it, and exits normally. The handled
%% exception must not surface as an exit exception to the embedder.
catch_then_normal() ->
    try erlang:throw(boom) catch throw:boom -> ok end.

%% A catch clause that matches no class makes the compiler emit the
%% raise instruction on the fallthrough path: the rethrown exception
%% must keep its original class (throw), not default to error.
rethrow_unmatched_class() ->
    try erlang:throw(boom) catch error:E -> {unexpected, E} end.
