import json
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from aiohttp import FormData, web
from aiohttp.test_utils import TestClient, TestServer


if "google_auth_oauthlib.flow" not in sys.modules:
    flow_module = types.ModuleType("google_auth_oauthlib.flow")

    class Flow:  # pragma: no cover - test import shim only
        @classmethod
        def from_client_config(cls, *args, **kwargs):
            raise RuntimeError("OAuth flow should not be used in artifact server tests")

    flow_module.Flow = Flow
    package_module = types.ModuleType("google_auth_oauthlib")
    package_module.flow = flow_module
    sys.modules["google_auth_oauthlib"] = package_module
    sys.modules["google_auth_oauthlib.flow"] = flow_module

if "google.oauth2.id_token" not in sys.modules:
    google_module = sys.modules.setdefault("google", types.ModuleType("google"))
    oauth2_module = types.ModuleType("google.oauth2")
    id_token_module = types.ModuleType("google.oauth2.id_token")
    id_token_module.verify_oauth2_token = lambda *args, **kwargs: {}
    oauth2_module.id_token = id_token_module
    google_module.oauth2 = oauth2_module
    sys.modules["google.oauth2"] = oauth2_module
    sys.modules["google.oauth2.id_token"] = id_token_module

if "google.auth.transport.requests" not in sys.modules:
    google_module = sys.modules.setdefault("google", types.ModuleType("google"))
    auth_module = types.ModuleType("google.auth")
    transport_module = types.ModuleType("google.auth.transport")
    requests_module = types.ModuleType("google.auth.transport.requests")

    class Request:  # pragma: no cover - test import shim only
        pass

    requests_module.Request = Request
    transport_module.requests = requests_module
    auth_module.transport = transport_module
    google_module.auth = auth_module
    sys.modules["google.auth"] = auth_module
    sys.modules["google.auth.transport"] = transport_module
    sys.modules["google.auth.transport.requests"] = requests_module


from src import orchestrator as orchestrator_module
from src.orchestrator import Orchestrator


class FakeOAuth:
    def __init__(self):
        self.trusted_networks = []

    def verify_jwt(self, token: str):
        if token == "good-token":
            return "engineer@rajeshgo.li"
        return None


class ArtifactServerTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)

    async def asyncTearDown(self):
        self.temp_dir.cleanup()

    async def _make_client(self) -> tuple[Orchestrator, TestClient]:
        orchestrator = Orchestrator.__new__(Orchestrator)
        orchestrator.oauth = FakeOAuth()
        orchestrator._artifacts_root = self.root / "data" / "apps"
        orchestrator._legacy_apk_path = self.root / "data" / "app-debug.apk"

        app = web.Application(
            middlewares=[Orchestrator._cors_middleware, orchestrator._oauth_middleware()],
            client_max_size=orchestrator_module.ARTIFACT_MAX_SIZE_BYTES + (1024 * 1024),
        )
        app.router.add_post("/deploy/{app}", orchestrator._handle_deploy_post)
        app.router.add_get("/apps/{app}/latest.apk", orchestrator._handle_app_artifact_get)
        app.router.add_get("/apk", orchestrator._handle_apk_get)

        server = TestServer(app)
        client = TestClient(server)
        await client.start_server()
        self.addAsyncCleanup(client.close)
        self.addAsyncCleanup(server.close)
        return orchestrator, client

    async def _upload(self, client: TestClient, *, body: bytes, token: str | None = "good-token"):
        form = FormData()
        form.add_field(
            "file",
            body,
            filename="artifact.apk",
            content_type="application/vnd.android.package-archive",
        )
        headers = {}
        if token is not None:
            headers["Authorization"] = f"Bearer {token}"
        return await client.post("/deploy/office-climate", data=form, headers=headers)

    async def test_upload_success_writes_artifact_and_metadata(self):
        _, client = await self._make_client()

        response = await self._upload(client, body=b"apk-bytes")
        payload = await response.json()

        self.assertEqual(response.status, 200)
        self.assertEqual(payload["download_url"], "/apps/office-climate/latest.apk")

        artifact_path = self.root / "data" / "apps" / "office-climate" / "latest.apk"
        metadata_path = self.root / "data" / "apps" / "office-climate" / "meta.json"
        self.assertTrue(artifact_path.exists())
        self.assertTrue(metadata_path.exists())
        self.assertEqual(artifact_path.read_bytes(), b"apk-bytes")

        metadata = json.loads(metadata_path.read_text())
        self.assertEqual(metadata["size_bytes"], len(b"apk-bytes"))
        self.assertEqual(metadata["uploaded_by"], "engineer@rajeshgo.li")
        self.assertIn("uploaded_at", metadata)

    async def test_download_returns_uploaded_artifact(self):
        _, client = await self._make_client()
        await self._upload(client, body=b"download-me")

        response = await client.get("/apps/office-climate/latest.apk")
        body = await response.read()

        self.assertEqual(response.status, 200)
        self.assertEqual(body, b"download-me")
        self.assertEqual(
            response.headers.get("Content-Disposition"),
            "attachment; filename=office-climate.apk",
        )

    async def test_upload_requires_authentication(self):
        _, client = await self._make_client()

        response = await self._upload(client, body=b"unauthorized", token=None)
        payload = await response.json()

        self.assertEqual(response.status, 401)
        self.assertEqual(payload["error"], "Authentication required")

    async def test_download_does_not_require_authentication(self):
        _, client = await self._make_client()
        await self._upload(client, body=b"public-download")

        response = await client.get("/apps/office-climate/latest.apk")

        self.assertEqual(response.status, 200)
        self.assertEqual(await response.read(), b"public-download")

    async def test_missing_artifact_returns_404(self):
        _, client = await self._make_client()

        response = await client.get("/apps/nonexistent/latest.apk")

        self.assertEqual(response.status, 404)

    async def test_upload_size_limit_returns_413(self):
        with patch.object(orchestrator_module, "ARTIFACT_MAX_SIZE_BYTES", 16):
            _, client = await self._make_client()
            response = await self._upload(client, body=b"x" * 17)
            payload = await response.json()

        self.assertEqual(response.status, 413)
        self.assertEqual(payload["error"], "Artifact exceeds 100 MB limit")
