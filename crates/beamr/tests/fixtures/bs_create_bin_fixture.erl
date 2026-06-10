%% Fixture exercising the bs_create_bin segment shapes emitted by real
%% compilers: append/private_append, sized and unsized binary segments,
%% string literals, integers with endianness flags, floats, utf8, and
%% sub-byte segments. Compile with `erlc bs_create_bin_fixture.erl`
%% (OTP 25+) and commit the .beam next to this source.
-module(bs_create_bin_fixture).
-export([concat_regs/0, lit_prefix/0, int8/0, int16_little/0, utf8_seg/0,
         join/0, case_concat/0, signed_little/0, sized_bin/0, big16/0,
         native16/0, big_signed/0, float64/0, float32/0, float_little/0,
         priv_append/0, bit_seg/0]).

%% <<A/binary, B/binary>> with both sources in registers.
concat_regs() ->
    A = id(<<"hello">>),
    B = id(<<" world">>),
    <<A/binary, B/binary>>.

%% String-table segment followed by a register binary.
lit_prefix() ->
    A = id(<<"tail">>),
    <<"lit", A/binary>>.

int8() ->
    X = id(65),
    <<X:8>>.

int16_little() ->
    X = id(16706),
    <<X:16/little>>.

utf8_seg() ->
    X = id(16#20AC),
    <<X/utf8, "!">>.

%% string:join-style fold over binaries (the Gleam string.join shape).
join() ->
    join_list(id([<<"a">>, <<"b">>, <<"c">>]), id(<<",">>)).

join_list([], _Sep) -> <<>>;
join_list([H | T], Sep) ->
    fold(fun(El, Acc) -> <<Acc/binary, Sep/binary, El/binary>> end, H, T).

fold(_F, Acc, []) -> Acc;
fold(F, Acc, [H | T]) -> fold(F, F(H, Acc), T).

%% Binary concatenation after a case expression (the failing Gleam shape).
case_concat() ->
    X = id(1),
    Prefix = case X of
        1 -> <<"one">>;
        _ -> <<"other">>
    end,
    Suffix = id(<<"-tail">>),
    <<Prefix/binary, Suffix/binary>>.

signed_little() ->
    X = id(-2),
    <<X:16/little-signed>>.

sized_bin() ->
    A = id(<<"abcdef">>),
    N = id(3),
    <<A:N/binary>>.

big16() ->
    X = id(513),
    <<X:16/big>>.

native16() ->
    X = id(513),
    <<X:16/native>>.

big_signed() ->
    X = id(-513),
    <<X:16/big-signed>>.

float64() ->
    X = id(3.14),
    <<X:64/float>>.

float32() ->
    X = id(1.5),
    <<X:32/float>>.

float_little() ->
    X = id(3.14),
    <<X:64/float-little>>.

%% bs_init_writable + private_append loop.
priv_append() ->
    priv_append_loop(id([1, 2, 3]), <<>>).

priv_append_loop([], Acc) -> Acc;
priv_append_loop([H | T], Acc) -> priv_append_loop(T, <<Acc/binary, H:8>>).

%% Sub-byte segments that pack into one whole byte.
bit_seg() ->
    X = id(5),
    <<X:3, 0:5>>.

id(X) -> X.
