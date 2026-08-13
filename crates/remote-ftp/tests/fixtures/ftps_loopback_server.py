#!/usr/bin/env python3
import argparse
import os
import socket
import ssl
import sys
import threading


MAX_COMMAND_BYTES = 4096
MAX_COMMANDS = 64
MAX_CONNECTIONS = 64
MAX_DATA_BYTES = 1024 * 1024
IO_TIMEOUT_SECONDS = 3
USERNAME = "localdesk"
PASSWORD = "loopback-password"


class State:
    def __init__(self, events_path):
        self.lock = threading.Lock()
        self.events_path = events_path
        self.directories = {"/"}
        self.files = {
            "/source.txt": b"loopback-source",
            "/resumed.txt.part": b"explicit",
        }

    def event(self, value):
        with self.lock:
            with open(self.events_path, "a", encoding="ascii") as output:
                output.write(value + "\n")
                output.flush()
        if value.startswith("ERROR "):
            print(value, file=sys.stderr, flush=True)


def normalize_path(cwd, argument):
    if not argument:
        combined = cwd
    elif argument.startswith("/"):
        combined = argument
    elif cwd == "/":
        combined = "/" + argument
    else:
        combined = cwd + "/" + argument
    components = []
    for component in combined.split("/"):
        if component in ("", "."):
            continue
        if component == "..":
            if components:
                components.pop()
            continue
        if any(ord(character) < 32 for character in component):
            raise ValueError("path contains control characters")
        components.append(component)
    return "/" + "/".join(components) if components else "/"


def parent_path(path):
    parent, _, _ = path.rpartition("/")
    return parent or "/"


def send_reply(control, line):
    control.write((line + "\r\n").encode("ascii"))
    control.flush()


class Session:
    def __init__(self, connection, context, state):
        self.connection = connection
        self.context = context
        self.state = state
        self.control = connection.makefile("rwb", buffering=0)
        self.tls_active = False
        self.authenticated = False
        self.accepted_user = False
        self.pbsz_zero = False
        self.private_data = False
        self.cwd = "/"
        self.passive = None
        self.restart = 0
        self.rename_from = None
        self.last_verb = "BANNER"

    def close(self):
        if self.passive is not None:
            self.passive.close()
            self.passive = None
        try:
            self.control.close()
        except OSError:
            pass
        try:
            self.connection.close()
        except OSError:
            pass

    def enter_tls(self):
        send_reply(self.control, "234 AUTH TLS accepted")
        self.state.event("REPLY 234 AUTH TLS")
        self.control.close()
        self.connection = self.context.wrap_socket(self.connection, server_side=True)
        self.connection.settimeout(IO_TIMEOUT_SECONDS)
        self.control = self.connection.makefile("rwb", buffering=0)
        self.tls_active = True
        self.state.event("CONTROL_TLS")

    def enter_passive(self):
        if self.passive is not None:
            self.passive.close()
        self.passive = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.passive.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.passive.bind(("127.0.0.1", 0))
        self.passive.listen(1)
        self.passive.settimeout(IO_TIMEOUT_SECONDS)
        port = self.passive.getsockname()[1]
        send_reply(self.control, f"229 Entering Extended Passive Mode (|||{port}|)")

    def open_data(self, verb):
        if self.passive is None:
            raise RuntimeError("passive mode was not entered")
        send_reply(self.control, "150 Opening protected data connection")
        connection, peer = self.passive.accept()
        self.passive.close()
        self.passive = None
        if peer[0] != "127.0.0.1":
            connection.close()
            raise RuntimeError("non-loopback data peer")
        connection.settimeout(IO_TIMEOUT_SECONDS)
        if not self.private_data:
            connection.close()
            raise RuntimeError("data channel was not PROT P")
        protected = self.context.wrap_socket(connection, server_side=True)
        protected.settimeout(IO_TIMEOUT_SECONDS)
        self.state.event("DATA_TLS " + verb)
        return protected

    def send_data(self, verb, payload):
        if len(payload) > MAX_DATA_BYTES:
            raise RuntimeError("data byte limit exceeded")
        data = self.open_data(verb)
        try:
            data.sendall(payload)
            raw = data.unwrap()
            raw.close()
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
            data.close()
        send_reply(self.control, "226 Transfer complete")

    def receive_data(self, verb):
        data = self.open_data(verb)
        payload = bytearray()
        try:
            while len(payload) <= MAX_DATA_BYTES:
                chunk = data.recv(min(65536, MAX_DATA_BYTES + 1 - len(payload)))
                if not chunk:
                    break
                payload.extend(chunk)
            raw = data.unwrap()
            raw.close()
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
            data.close()
        finally:
            data.close()
        if len(payload) > MAX_DATA_BYTES:
            raise RuntimeError("data byte limit exceeded")
        send_reply(self.control, "226 Transfer complete")
        return bytes(payload)

    def machine_list_directory(self, directory):
        with self.state.lock:
            if directory not in self.state.directories:
                return None
            entries = [
                (path.rsplit("/", 1)[-1], None)
                for path in self.state.directories
                if path != directory and parent_path(path) == directory
            ]
            entries.extend(
                (path.rsplit("/", 1)[-1], len(payload))
                for path, payload in self.state.files.items()
                if parent_path(path) == directory
            )
        lines = (
            f"type=dir; {name}\r\n" if size is None else f"type=file;size={size}; {name}\r\n"
            for name, size in sorted(entries)
        )
        return "".join(lines).encode("utf-8")

    def run(self):
        send_reply(self.control, "220 LocalDesk loopback explicit FTPS ready")
        self.state.event("REPLY 220")
        for _ in range(MAX_COMMANDS):
            line = self.control.readline(MAX_COMMAND_BYTES + 1)
            if not line:
                return
            if len(line) > MAX_COMMAND_BYTES or not line.endswith(b"\n"):
                raise RuntimeError("control line limit exceeded")
            decoded = line.rstrip(b"\r\n").decode("utf-8")
            verb, _, argument = decoded.partition(" ")
            verb = verb.upper()
            argument = argument.strip()
            self.last_verb = verb
            self.state.event(verb + (" " + argument.upper() if verb in {"AUTH", "PBSZ", "PROT"} else ""))

            if verb == "AUTH":
                if self.tls_active or argument.upper() != "TLS":
                    send_reply(self.control, "534 AUTH TLS required")
                else:
                    self.enter_tls()
                continue
            if not self.tls_active:
                send_reply(self.control, "534 TLS required")
                continue
            if verb == "USER":
                self.accepted_user = argument == USERNAME
                send_reply(self.control, "331 Password required" if self.accepted_user else "530 Authentication failed")
                continue
            if verb == "PASS":
                self.authenticated = self.accepted_user and argument == PASSWORD
                send_reply(self.control, "230 Login successful" if self.authenticated else "530 Authentication failed")
                continue
            if not self.authenticated and verb != "QUIT":
                send_reply(self.control, "530 Authentication required")
                continue

            if verb == "PBSZ":
                self.pbsz_zero = argument == "0"
                send_reply(self.control, "200 PBSZ=0" if self.pbsz_zero else "501 PBSZ 0 required")
                if self.pbsz_zero:
                    self.state.event("REPLY 200 PBSZ 0")
            elif verb == "PROT":
                self.private_data = self.pbsz_zero and argument.upper() == "P"
                send_reply(self.control, "200 PROT P" if self.private_data else "536 PROT P required")
                if self.private_data:
                    self.state.event("REPLY 200 PROT P")
            elif verb == "SYST":
                send_reply(self.control, "215 UNIX Type: L8")
            elif verb == "FEAT":
                for response in ("211-Features", " AUTH TLS", " PBSZ", " PROT", " EPSV", " SIZE", " REST STREAM", "211 End"):
                    send_reply(self.control, response)
            elif verb in {"PWD", "XPWD"}:
                send_reply(self.control, f'257 "{self.cwd}"')
            elif verb == "CWD":
                path = normalize_path(self.cwd, argument)
                with self.state.lock:
                    exists = path in self.state.directories
                if exists:
                    self.cwd = path
                send_reply(self.control, "250 Directory changed" if exists else "550 Directory unavailable")
            elif verb == "CDUP":
                self.cwd = parent_path(self.cwd)
                send_reply(self.control, "250 Directory changed")
            elif verb in {"TYPE", "OPTS", "CLNT", "NOOP"}:
                send_reply(self.control, "200 Command accepted")
            elif verb == "EPSV":
                self.enter_passive()
            elif verb == "PASV":
                send_reply(self.control, "522 Use EPSV")
            elif verb == "MLSD":
                payload = self.machine_list_directory(normalize_path(self.cwd, argument))
                if payload is None:
                    send_reply(self.control, "550 Directory unavailable")
                else:
                    self.send_data("MLSD", payload)
            elif verb == "SIZE":
                path = normalize_path(self.cwd, argument)
                with self.state.lock:
                    payload = self.state.files.get(path)
                send_reply(self.control, f"213 {len(payload)}" if payload is not None else "550 File unavailable")
            elif verb == "REST":
                try:
                    offset = int(argument)
                except ValueError:
                    offset = -1
                if 0 <= offset <= MAX_DATA_BYTES:
                    self.restart = offset
                    send_reply(self.control, "350 Restart position accepted")
                else:
                    send_reply(self.control, "501 Invalid restart position")
            elif verb == "RETR":
                path = normalize_path(self.cwd, argument)
                with self.state.lock:
                    payload = self.state.files.get(path)
                if payload is None or self.restart > len(payload):
                    send_reply(self.control, "550 File unavailable")
                else:
                    self.send_data("RETR", payload[self.restart :])
                self.restart = 0
            elif verb in {"STOR", "APPE"}:
                path = normalize_path(self.cwd, argument)
                payload = self.receive_data(verb)
                with self.state.lock:
                    if verb == "APPE":
                        payload = self.state.files.get(path, b"") + payload
                    if len(payload) > MAX_DATA_BYTES:
                        raise RuntimeError("file byte limit exceeded")
                    self.state.files[path] = payload
            elif verb in {"MKD", "XMKD"}:
                path = normalize_path(self.cwd, argument)
                with self.state.lock:
                    created = len(self.state.directories) < 32 and path not in self.state.directories
                    if created:
                        self.state.directories.add(path)
                send_reply(self.control, f'257 "{path}" created' if created else "550 Directory unavailable")
            elif verb in {"RMD", "XRMD"}:
                path = normalize_path(self.cwd, argument)
                with self.state.lock:
                    empty = not any(parent_path(item) == path for item in self.state.files)
                    empty = empty and not any(item != path and parent_path(item) == path for item in self.state.directories)
                    removed = path != "/" and empty and path in self.state.directories
                    if removed:
                        self.state.directories.remove(path)
                send_reply(self.control, "250 Directory removed" if removed else "550 Directory unavailable")
            elif verb == "DELE":
                path = normalize_path(self.cwd, argument)
                with self.state.lock:
                    removed = self.state.files.pop(path, None) is not None
                send_reply(self.control, "250 File deleted" if removed else "550 File unavailable")
            elif verb == "RNFR":
                path = normalize_path(self.cwd, argument)
                with self.state.lock:
                    exists = path in self.state.files or path in self.state.directories
                self.rename_from = path if exists else None
                send_reply(self.control, "350 Rename source accepted" if exists else "550 Path unavailable")
            elif verb == "RNTO":
                if self.rename_from is None:
                    send_reply(self.control, "503 RNFR required")
                    continue
                destination = normalize_path(self.cwd, argument)
                with self.state.lock:
                    source = self.rename_from
                    if source in self.state.files:
                        self.state.files[destination] = self.state.files.pop(source)
                        renamed = True
                    elif source in self.state.directories:
                        self.state.directories.remove(source)
                        self.state.directories.add(destination)
                        renamed = True
                    else:
                        renamed = False
                self.rename_from = None
                send_reply(self.control, "250 Rename complete" if renamed else "550 Path unavailable")
            elif verb == "ABOR":
                self.restart = 0
                send_reply(self.control, "226 Abort complete")
            elif verb == "QUIT":
                send_reply(self.control, "221 Goodbye")
                return
            else:
                send_reply(self.control, "502 Command not implemented")
                raise RuntimeError("unexpected command " + verb)
        raise RuntimeError("command limit exceeded")


def serve(connection, context, state):
    connection.settimeout(IO_TIMEOUT_SECONDS)
    session = Session(connection, context, state)
    try:
        session.run()
    except (BrokenPipeError, ConnectionResetError, socket.timeout, ssl.SSLError) as error:
        state.event("PEER_CLOSED " + type(error).__name__ + " " + session.last_verb)
    except Exception as error:
        state.event("ERROR " + type(error).__name__ + " " + str(error))
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--certificate", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--ready", required=True)
    parser.add_argument("--events", required=True)
    arguments = parser.parse_args()

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(arguments.certificate, arguments.key)
    state = State(arguments.events)

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(16)
    port = listener.getsockname()[1]
    with open(arguments.ready, "x", encoding="ascii") as ready:
        ready.write(str(port))
        ready.flush()
        os.fsync(ready.fileno())

    for _ in range(MAX_CONNECTIONS):
        connection, peer = listener.accept()
        if peer[0] != "127.0.0.1":
            connection.close()
            continue
        threading.Thread(target=serve, args=(connection, context, state), daemon=True).start()


if __name__ == "__main__":
    main()
