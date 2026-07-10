-module(r9p_beam_port_ffi).

-export([
    decode_hex/1,
    encode_hex/1,
    front_request/3,
    request/3
]).

-define(CLIENT_SERVER, r9p_beam_client_port_server).
-define(FRONT_SERVER, r9p_beam_front_port_server).
-define(LINE_LIMIT, 16777216).

request(Executable, Line, TimeoutMs) ->
    request_on(?CLIENT_SERVER, Executable, Line, TimeoutMs).

front_request(Executable, Line, TimeoutMs) ->
    request_on(?FRONT_SERVER, Executable, Line, TimeoutMs).

request_on(ServerName, Executable, Line, TimeoutMs) ->
    case ensure_server(ServerName, Executable) of
        {ok, Server} ->
            Ref = make_ref(),
            Server ! {request, self(), Ref, Line, TimeoutMs},
            receive
                {Ref, Result} ->
                    Result
            after TimeoutMs + 1000 ->
                {error, <<"r9p_beam_port_timeout">>}
            end;
        {error, Reason} ->
            {error, Reason}
    end.

ensure_server(ServerName, Executable) ->
    case whereis(ServerName) of
        undefined ->
            start_server(ServerName, Executable);
        Pid ->
            {ok, Pid}
    end.

start_server(ServerName, Executable) ->
    case resolve_executable(Executable) of
        {ok, Resolved} ->
            Pid = spawn(fun() -> server_loop(Resolved, start_port(Resolved)) end),
            case catch register(ServerName, Pid) of
                true ->
                    {ok, Pid};
                _ ->
                    case whereis(ServerName) of
                        undefined ->
                            {ok, Pid};
                        Existing ->
                            Pid ! stop,
                            {ok, Existing}
                    end
            end;
        {error, Reason} ->
            {error, Reason}
    end.

resolve_executable(Executable) ->
    Chars = binary_to_list(Executable),
    case os:find_executable(Chars) of
        false ->
            case filelib:is_file(Chars) of
                true -> {ok, Executable};
                false -> {error, <<"r9p_beam_port_executable_not_found:", Executable/binary>>}
            end;
        Path ->
            {ok, unicode:characters_to_binary(Path)}
    end.

start_port(Executable) ->
    try
        Port = open_port(
            {spawn_executable, binary_to_list(Executable)},
            [
                binary,
                exit_status,
                use_stdio,
                hide,
                {line, ?LINE_LIMIT}
            ]
        ),
        {ok, Port}
    catch
        _:Reason ->
            {error, format_reason(Reason)}
    end.

server_loop(Executable, PortState) ->
    receive
        {request, From, Ref, Line, TimeoutMs} ->
            {Reply, NextPortState} =
                handle_request(Executable, PortState, Line, TimeoutMs),
            From ! {Ref, Reply},
            server_loop(Executable, NextPortState);
        stop ->
            close_port_state(PortState),
            ok
    end.

handle_request(Executable, {error, Reason}, _Line, _TimeoutMs) ->
    {{error, <<"r9p_beam_port_start_failed:", Reason/binary>>}, start_port(Executable)};
handle_request(Executable, {ok, Port}, Line, TimeoutMs) ->
    case catch port_command(Port, <<Line/binary, "\n">>) of
        true ->
            case await_response(Port, deadline(TimeoutMs), <<>>) of
                {reply, Reply, keep} ->
                    {Reply, {ok, Port}};
                {reply, Reply, restart} ->
                    close_port_state({ok, Port}),
                    {Reply, start_port(Executable)}
            end;
        _ ->
            close_port_state({ok, Port}),
            {
                {error, <<"r9p_beam_port_command_failed">>},
                start_port(Executable)
            }
    end.

await_response(Port, Deadline, Buffer) ->
    Remaining = remaining_timeout(Deadline),
    receive
        {Port, {data, {eol, Line}}} ->
            parse_response(<<Buffer/binary, Line/binary>>);
        {Port, {data, {noeol, Line}}} ->
            await_response(Port, Deadline, <<Buffer/binary, Line/binary>>);
        {Port, {data, Line}} ->
            parse_response(<<Buffer/binary, Line/binary>>);
        {Port, {exit_status, Status}} ->
            {
                reply,
                {error, <<"r9p_beam_port_exit:", (integer_to_binary(Status))/binary>>},
                restart
            }
    after Remaining ->
        {reply, {error, <<"r9p_beam_port_timeout">>}, restart}
    end.

parse_response(<<"ok\t", PayloadHex/binary>>) ->
    case decode_hex(PayloadHex) of
        {ok, Payload} -> {reply, {ok, Payload}, keep};
        {error, Reason} -> {reply, {error, Reason}, keep}
    end;
parse_response(<<"error\t", ReasonHex/binary>>) ->
    case decode_hex(ReasonHex) of
        {ok, Reason} -> {reply, {error, Reason}, keep};
        {error, DecodeReason} -> {reply, {error, DecodeReason}, keep}
    end;
parse_response(Other) ->
    {reply, {error, <<"r9p_beam_port_unexpected_response:", Other/binary>>}, keep}.

close_port_state({ok, Port}) ->
    catch erlang:port_close(Port),
    ok;
close_port_state({error, _}) ->
    ok.

deadline(TimeoutMs) ->
    erlang:monotonic_time(millisecond) + TimeoutMs.

remaining_timeout(Deadline) ->
    Remaining = Deadline - erlang:monotonic_time(millisecond),
    case Remaining > 0 of
        true -> Remaining;
        false -> 0
    end.

format_reason(Reason) ->
    unicode:characters_to_binary(io_lib:format("~p", [Reason])).

encode_hex(Value) when is_binary(Value) ->
    << <<(hex_digit(Byte bsr 4)), (hex_digit(Byte band 16#0f))>>
       || <<Byte:8>> <= Value >>.

decode_hex(Value) when is_binary(Value) ->
    case byte_size(Value) rem 2 of
        0 ->
            decode_hex(Value, <<>>);
        _ ->
            {error, <<"odd_hex_length">>}
    end.

decode_hex(<<>>, Acc) ->
    {ok, Acc};
decode_hex(<<High:8, Low:8, Rest/binary>>, Acc) ->
    case {hex_value(High), hex_value(Low)} of
        {{ok, H}, {ok, L}} ->
            decode_hex(Rest, <<Acc/binary, (H bsl 4 bor L)>>);
        {{error, Reason}, _} ->
            {error, Reason};
        {_, {error, Reason}} ->
            {error, Reason}
    end.

hex_digit(Value) when Value < 10 ->
    $0 + Value;
hex_digit(Value) ->
    $a + Value - 10.

hex_value(Value) when Value >= $0, Value =< $9 ->
    {ok, Value - $0};
hex_value(Value) when Value >= $a, Value =< $f ->
    {ok, Value - $a + 10};
hex_value(Value) when Value >= $A, Value =< $F ->
    {ok, Value - $A + 10};
hex_value(_) ->
    {error, <<"invalid_hex_digit">>}.
