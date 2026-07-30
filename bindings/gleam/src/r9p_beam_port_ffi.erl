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
            Pid = spawn(fun() ->
                server_loop(Resolved, start_port(Resolved), #{}, <<>>)
            end),
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

server_loop(Executable, PortState, Pending, Buffer) ->
    receive
        {request, From, Ref, Line, TimeoutMs} ->
            case send_request(Executable, PortState, Line, TimeoutMs) of
                {ok, RequestId, Timer, NextPortState} ->
                    NextPending = maps:put(
                        RequestId,
                        {From, Ref, Timer},
                        Pending
                    ),
                    server_loop(Executable, NextPortState, NextPending, Buffer);
                {error, Reason, NextPortState} ->
                    From ! {Ref, {error, Reason}},
                    fail_pending(Pending, Reason),
                    server_loop(Executable, NextPortState, #{}, <<>>)
            end;
        {request_timeout, RequestId} ->
            case maps:take(RequestId, Pending) of
                error ->
                    server_loop(Executable, PortState, Pending, Buffer);
                {{From, Ref, _Timer}, Remaining} ->
                    From ! {Ref, {error, <<"r9p_beam_port_timeout">>}},
                    fail_pending(Remaining, <<"r9p_beam_port_timeout">>),
                    close_port_state(PortState),
                    server_loop(Executable, start_port(Executable), #{}, <<>>)
            end;
        {Port, {data, {noeol, Line}}} ->
            case is_current_port(PortState, Port) of
                true ->
                    server_loop(
                        Executable,
                        PortState,
                        Pending,
                        <<Buffer/binary, Line/binary>>
                    );
                false ->
                    server_loop(Executable, PortState, Pending, Buffer)
            end;
        {Port, {data, {eol, Line}}} ->
            handle_response_line(
                Executable,
                PortState,
                Port,
                Pending,
                Buffer,
                Line
            );
        {Port, {data, Line}} ->
            handle_response_line(
                Executable,
                PortState,
                Port,
                Pending,
                Buffer,
                Line
            );
        {Port, {exit_status, Status}} ->
            case is_current_port(PortState, Port) of
                true ->
                    Reason = <<
                        "r9p_beam_port_exit:",
                        (integer_to_binary(Status))/binary
                    >>,
                    fail_pending(Pending, Reason),
                    close_port_state(PortState),
                    server_loop(Executable, start_port(Executable), #{}, <<>>);
                false ->
                    server_loop(Executable, PortState, Pending, Buffer)
            end;
        stop ->
            close_port_state(PortState),
            fail_pending(Pending, <<"r9p_beam_port_stopped">>),
            ok
    end.

send_request(Executable, {error, Reason}, _Line, _TimeoutMs) ->
    {
        error,
        <<"r9p_beam_port_start_failed:", Reason/binary>>,
        start_port(Executable)
    };
send_request(Executable, {ok, Port} = PortState, Line, TimeoutMs) ->
    RequestId = erlang:unique_integer([monotonic, positive]),
    Envelope = <<
        (integer_to_binary(RequestId))/binary,
        "\t",
        Line/binary,
        "\n"
    >>,
    case catch port_command(Port, Envelope) of
        true ->
            Timer = erlang:send_after(
                TimeoutMs,
                self(),
                {request_timeout, RequestId}
            ),
            {ok, RequestId, Timer, PortState};
        _ ->
            close_port_state(PortState),
            {
                error,
                <<"r9p_beam_port_command_failed">>,
                start_port(Executable)
            }
    end.

handle_response_line(Executable, PortState, Port, Pending, Buffer, Line) ->
    case is_current_port(PortState, Port) of
        false ->
            server_loop(Executable, PortState, Pending, Buffer);
        true ->
            case parse_tagged_response(<<Buffer/binary, Line/binary>>) of
                {ok, RequestId, Reply} ->
                    case maps:take(RequestId, Pending) of
                        error ->
                            server_loop(Executable, PortState, Pending, <<>>);
                        {{From, Ref, Timer}, Remaining} ->
                            _ = erlang:cancel_timer(Timer),
                            From ! {Ref, Reply},
                            server_loop(
                                Executable,
                                PortState,
                                Remaining,
                                <<>>
                            )
                    end;
                {error, Reason} ->
                    fail_pending(Pending, Reason),
                    close_port_state(PortState),
                    server_loop(Executable, start_port(Executable), #{}, <<>>)
            end
    end.

parse_tagged_response(Line) ->
    case binary:match(Line, <<"\t">>) of
        {Position, 1} ->
            <<RequestIdBinary:Position/binary, _:8, Response/binary>> = Line,
            try binary_to_integer(RequestIdBinary) of
                RequestId ->
                    {ok, RequestId, parse_response(Response)}
            catch
                _:_ ->
                    {error, <<"r9p_beam_port_invalid_response_id">>}
            end;
        nomatch ->
            {error, <<"r9p_beam_port_missing_response_id">>}
    end.

parse_response(<<"ok\t", PayloadHex/binary>>) ->
    case decode_hex(PayloadHex) of
        {ok, Payload} -> {ok, Payload};
        {error, Reason} -> {error, Reason}
    end;
parse_response(<<"error\t", ReasonHex/binary>>) ->
    case decode_hex(ReasonHex) of
        {ok, Reason} -> {error, Reason};
        {error, DecodeReason} -> {error, DecodeReason}
    end;
parse_response(Other) ->
    {error, <<"r9p_beam_port_unexpected_response:", Other/binary>>}.

is_current_port({ok, Current}, Candidate) ->
    Current =:= Candidate;
is_current_port({error, _}, _Candidate) ->
    false.

fail_pending(Pending, Reason) ->
    maps:foreach(
        fun(_RequestId, {From, Ref, Timer}) ->
            _ = erlang:cancel_timer(Timer),
            From ! {Ref, {error, Reason}}
        end,
        Pending
    ).

close_port_state({ok, Port}) ->
    catch erlang:port_close(Port),
    ok;
close_port_state({error, _}) ->
    ok.

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
