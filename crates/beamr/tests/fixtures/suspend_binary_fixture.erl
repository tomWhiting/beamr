%% Fixture for the suspension-result binary regression tests.
%%
%% `run/1` mirrors the aion workflow shape: a native call suspends the
%% process (message-wakeable `request_suspend`), the embedder wakes it with
%% a mailbox marker, the native re-executes and returns `{ok, Binary}`
%% built on the process heap, and the workflow code then *uses* the binary
%% (size, head byte, rest matching) like Gleam-generated decode code does.
-module(suspend_binary_fixture).
-export([run/1]).

run(Size) ->
    case beamr_suspend_binary_test:await_payload(Size) of
        {ok, Bin} when is_binary(Bin) ->
            consume(Bin);
        Other ->
            {unexpected, Other}
    end.

consume(Bin) ->
    Size = byte_size(Bin),
    <<First, Rest/binary>> = Bin,
    {ok, Size, First, byte_size(Rest)}.
