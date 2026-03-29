import json
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from aiohttp import FormData, web
from aiohttp.test_utils import TestClient, TestServer


if "aiomqtt" not in sys.modules:
    aiomqtt_module = types.ModuleType("aiomqtt")
    aiomqtt_module.Message = object
    aiomqtt_module.Client = object
    sys.modules["aiomqtt"] = aiomqtt_module

if "tinytuya" not in sys.modules:
    tinytuya_module = types.ModuleType("tinytuya")
    tinytuya_module.Device = object
    tinytuya_module.Cloud = object
    sys.modules["tinytuya"] = tinytuya_module

if "jwt" not in sys.modules:
    jwt_module = types.ModuleType("jwt")

    class ExpiredSignatureError(Exception):
        pass

    class InvalidTokenError(Exception):
        pass

    jwt_module.decode = lambda *args, **kwargs: {}
    jwt_module.encode = lambda *args, **kwargs: "token"
    jwt_module.ExpiredSignatureError = ExpiredSignatureError
    jwt_module.InvalidTokenError = InvalidTokenError
    sys.modules["jwt"] = jwt_module

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
        self.redirect_uri = "http://configured.example/auth/callback"
        self.fail_authorization = False

    def verify_jwt(self, token: str):
        if token == "good-token":
            return "engineer@rajeshgo.li"
        return None

    def generate_pkce_pair(self):
        return ("verifier", "challenge")

    def create_authorization_url(self, state: str, code_challenge: str):
        if self.fail_authorization:
            raise RuntimeError("boom")
        return (
            "https://accounts.example/authorize"
            f"?state={state}"
            f"&challenge={code_challenge}"
            f"&redirect_uri={self.redirect_uri}"
        )


class ArtifactServerTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)

    async def asyncTearDown(self):
        self.temp_dir.cleanup()

    async def _make_client(self) -> tuple[Orchestrator, TestClient]:
        orchestrator = Orchestrator.__new__(Orchestrator)
        orchestrator.oauth = FakeOAuth()
        orchestrator._oauth_states = {}
        orchestrator._artifacts_root = self.root / "data" / "apps"
        orchestrator._legacy_apk_path = self.root / "data" / "app-debug.apk"

        app = web.Application(
            middlewares=[Orchestrator._cors_middleware, orchestrator._oauth_middleware()],
            client_max_size=orchestrator_module.ARTIFACT_MAX_SIZE_BYTES + (1024 * 1024),
        )
        app.router.add_post("/deploy/{app}", orchestrator._handle_deploy_post)
        app.router.add_get("/apps/{app}/latest.apk", orchestrator._handle_app_artifact_get)
        app.router.add_get("/apps/{app}/meta.json", orchestrator._handle_app_artifact_meta_get)
        app.router.add_get("/apk", orchestrator._handle_apk_get)

        server = TestServer(app)
        client = TestClient(server)
        await client.start_server()
        self.addAsyncCleanup(client.close)
        self.addAsyncCleanup(server.close)
        return orchestrator, client

    async def _make_auth_client(self) -> tuple[Orchestrator, TestClient]:
        orchestrator = Orchestrator.__new__(Orchestrator)
        orchestrator.oauth = FakeOAuth()
        orchestrator._oauth_states = {}

        app = web.Application()
        app.router.add_get("/auth/login", orchestrator._handle_auth_login)

        server = TestServer(app)
        client = TestClient(server)
        await client.start_server()
        self.addAsyncCleanup(client.close)
        self.addAsyncCleanup(server.close)
        return orchestrator, client

    async def _upload(
        self,
        client: TestClient,
        *,
        body: bytes,
        token: str | None = "good-token",
        version_code: int | None = None,
        version_name: str | None = None,
    ):
        form = FormData()
        form.add_field(
            "file",
            body,
            filename="artifact.apk",
            content_type="application/vnd.android.package-archive",
        )
        if version_code is not None:
            form.add_field("version_code", str(version_code))
        if version_name is not None:
            form.add_field("version_name", version_name)
        headers = {}
        if token is not None:
            headers["Authorization"] = f"Bearer {token}"
        return await client.post("/deploy/office-climate", data=form, headers=headers)

    async def test_upload_success_writes_artifact_and_metadata(self):
        _, client = await self._make_client()

        response = await self._upload(
            client,
            body=b"apk-bytes",
            version_code=7,
            version_name="1.2.0",
        )
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
        self.assertEqual(metadata["version_code"], 7)
        self.assertEqual(metadata["version_name"], "1.2.0")
        self.assertIn("uploaded_at", metadata)

    async def test_meta_endpoint_returns_uploaded_metadata(self):
        _, client = await self._make_client()
        await self._upload(client, body=b"meta-bytes", version_code=9, version_name="2.0")

        response = await client.get("/apps/office-climate/meta.json")
        payload = await response.json()

        self.assertEqual(response.status, 200)
        self.assertEqual(payload["size_bytes"], len(b"meta-bytes"))
        self.assertEqual(payload["version_code"], 9)
        self.assertEqual(payload["version_name"], "2.0")
        self.assertEqual(payload["uploaded_by"], "engineer@rajeshgo.li")

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

    async def test_missing_metadata_returns_404(self):
        _, client = await self._make_client()

        response = await client.get("/apps/nonexistent/meta.json")

        self.assertEqual(response.status, 404)

    async def test_upload_rejects_invalid_version_code(self):
        _, client = await self._make_client()

        form = FormData()
        form.add_field(
            "file",
            b"apk-bytes",
            filename="artifact.apk",
            content_type="application/vnd.android.package-archive",
        )
        form.add_field("version_code", "abc")

        response = await client.post(
            "/deploy/office-climate",
            data=form,
            headers={"Authorization": "Bearer good-token"},
        )
        payload = await response.json()

        self.assertEqual(response.status, 400)
        self.assertEqual(payload["error"], "version_code must be an integer")

    async def test_upload_size_limit_returns_413(self):
        with patch.object(orchestrator_module, "ARTIFACT_MAX_SIZE_BYTES", 16):
            _, client = await self._make_client()
            response = await self._upload(client, body=b"x" * 17)
            payload = await response.json()

        self.assertEqual(response.status, 413)
        self.assertEqual(payload["error"], "Artifact exceeds 100 MB limit")

    async def test_auth_login_android_returns_json_and_persists_platform(self):
        orchestrator, client = await self._make_auth_client()

        response = await client.get(
            "/auth/login?platform=android",
            headers={"Host": "office.rajeshgo.li"},
        )
        payload = await response.json()

        self.assertEqual(response.status, 200)
        self.assertIn("authorization_url", payload)
        self.assertIn("state", payload)
        self.assertEqual(
            orchestrator._oauth_redirect_uris[payload["state"]],
            "https://office.rajeshgo.li/auth/callback",
        )
        self.assertEqual(orchestrator._oauth_platforms[payload["state"]], "android")
        self.assertEqual(orchestrator.oauth.redirect_uri, "http://configured.example/auth/callback")

    async def test_auth_login_returns_json_error_when_authorization_fails(self):
        orchestrator, client = await self._make_auth_client()
        orchestrator.oauth.fail_authorization = True

        response = await client.get("/auth/login?platform=android")
        payload = await response.json()

        self.assertEqual(response.status, 500)
        self.assertEqual(payload["error"], "Failed to start OAuth")
        self.assertEqual(orchestrator._oauth_states, {})
