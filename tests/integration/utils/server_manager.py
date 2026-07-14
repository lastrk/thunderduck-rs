"""
Server Manager for Thunderduck Rust Spark Connect Integration Tests

Manages the lifecycle of the Thunderduck Rust server for integration testing.
Adapted from the Java reference: launches the Rust binary instead of a JVM JAR.
"""

import os
import signal
import socket
import subprocess
import time
from pathlib import Path

from port_utils import wait_for_port


class ServerManager:
    """Manages Thunderduck Rust Spark Connect server lifecycle for integration tests"""

    def __init__(self, host: str = "localhost", port: int = 15002):
        self.host = host
        self.port = port
        self.process: subprocess.Popen | None = None
        self.workspace_dir = Path(__file__).parent.parent.parent.parent

        # Allow overriding the binary path via environment variable
        binary_path = os.environ.get("THUNDERDUCK_BINARY")
        if binary_path:
            self.server_binary = Path(binary_path)
        else:
            # Default: release build output
            self.server_binary = (
                self.workspace_dir / "target/release/thunderduck-connect-server"
            )

    def is_port_available(self) -> bool:
        """Check if the port is available (tolerates TIME_WAIT connections)"""
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                s.bind((self.host, self.port))
                return True
            except OSError:
                return False

    def is_server_ready(self, timeout: int = 30) -> bool:
        """Check if server is ready to accept connections"""
        return wait_for_port(self.port, host=self.host, timeout=timeout)

    def start(self, timeout: int = 60) -> bool:
        """
        Start the Thunderduck Rust server.

        Args:
            timeout: Maximum time to wait for server to start (seconds)

        Returns:
            True if server started successfully, False otherwise
        """
        if not self.server_binary.exists():
            raise FileNotFoundError(
                f"Thunderduck binary not found: {self.server_binary}\n"
                f"Please build the project first: cargo build --release"
            )

        # Check if port is already in use
        if not self.is_port_available():
            print(f"Port {self.port} is already in use. Attempting to kill existing process...")
            self.kill_existing_server()
            time.sleep(2)

            if not self.is_port_available():
                raise RuntimeError(f"Port {self.port} is still in use after cleanup")

        # Build the command — Rust binary takes the port.
        cmd = [str(self.server_binary), "--port", str(self.port)]

        print(f"Starting Thunderduck server on {self.host}:{self.port}...")
        print(f"Command: {' '.join(cmd)}")

        # Redirect output to log files for debugging
        log_dir = self.workspace_dir / "tests/integration/logs"
        log_dir.mkdir(parents=True, exist_ok=True)

        stdout_file = log_dir / "server_stdout.log"
        stderr_file = log_dir / "server_stderr.log"

        env = os.environ.copy()

        with open(stdout_file, "w") as stdout, open(stderr_file, "w") as stderr:
            self.process = subprocess.Popen(
                cmd,
                cwd=str(self.workspace_dir),
                stdout=stdout,
                stderr=stderr,
                env=env,
                preexec_fn=os.setsid,  # New process group for clean shutdown
            )

        print(f"Server process started (PID: {self.process.pid})")
        print(f"Logs: stdout={stdout_file}, stderr={stderr_file}")

        # Wait for server to be ready
        if self.is_server_ready(timeout):
            print(f"✓ Server ready on {self.host}:{self.port}")
            return True
        else:
            print(f"✗ Server failed to start within {timeout} seconds")
            self.stop()
            if stderr_file.exists():
                print("\nLast 20 lines of stderr:")
                with open(stderr_file) as f:
                    lines = f.readlines()
                    for line in lines[-20:]:
                        print(f"  {line.rstrip()}")
            return False

    def stop(self):
        """Stop the Thunderduck server"""
        if self.process:
            print(f"Stopping server (PID: {self.process.pid})...")
            try:
                os.killpg(os.getpgid(self.process.pid), signal.SIGTERM)
                try:
                    self.process.wait(timeout=10)
                    print("✓ Server stopped gracefully")
                except subprocess.TimeoutExpired:
                    print("Server didn't stop gracefully, forcing kill...")
                    os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
                    self.process.wait()
                    print("✓ Server killed")
            except ProcessLookupError:
                print("Server process already terminated")
            except Exception as e:
                print(f"Error stopping server: {e}")
            finally:
                self.process = None

    def kill_existing_server(self):
        """Kill any existing server process on the port"""
        try:
            result = subprocess.run(
                ["lsof", "-ti", f":{self.port}"],
                capture_output=True,
                text=True,
            )
            if result.stdout.strip():
                for pid in result.stdout.strip().split("\n"):
                    try:
                        os.kill(int(pid), signal.SIGTERM)
                        print(f"Killed process {pid} using port {self.port}")
                    except (ProcessLookupError, ValueError):
                        pass
        except FileNotFoundError:
            try:
                subprocess.run(
                    ["ss", "-lptn", f"sport = :{self.port}"],
                    capture_output=True,
                    text=True,
                )
                print("Could not automatically kill process, please kill manually")
            except FileNotFoundError:
                print("Could not find process management tools")

    def __enter__(self):
        if not self.start():
            raise RuntimeError("Failed to start server")
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.stop()
