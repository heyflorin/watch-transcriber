import unittest
from unittest.mock import patch

import requests

import volc_lark


class FakeResponse:
    def __init__(
        self,
        *,
        payload=None,
        status_code=200,
        headers=None,
        text="",
        json_error=None,
    ):
        self._payload = payload
        self.status_code = status_code
        self.headers = headers or {}
        self.text = text
        self._json_error = json_error

    def json(self):
        if self._json_error is not None:
            raise self._json_error
        return self._payload


class LarkDurableJobTests(unittest.TestCase):
    request_id = "durable-request-123"
    task_id = "provider-task-456"
    file_url = "https://audio.example.test/object.mp3?signature=private"
    transcript_url = "https://results.example.test/transcript.json"

    def setUp(self):
        self.env = patch.dict("os.environ", {"VOLC_API_KEY": "test-api-key"})
        self.env.start()
        self.addCleanup(self.env.stop)

    @staticmethod
    def _submit_response(task_id="provider-task-456"):
        return FakeResponse(
            headers={"X-Api-Status-Code": "20000000"},
            payload={"Data": {"TaskID": task_id}},
        )

    def _success_query_response(self):
        return FakeResponse(
            payload={
                "Data": {
                    "Status": "success",
                    "Result": {
                        "AudioTranscriptionFile": self.transcript_url,
                    },
                }
            }
        )

    @staticmethod
    def _transcript_response():
        return FakeResponse(
            payload=[
                {
                    "speaker": {"id": "1"},
                    "content": "hello",
                    "start_time": 0,
                    "end_time": 1000,
                }
            ]
        )

    def test_submit_uses_caller_request_id_and_returns_task_id(self):
        with patch.object(
            volc_lark.requests,
            "post",
            return_value=self._submit_response(),
        ) as post:
            task_id = volc_lark.submit_lark_job(
                self.file_url,
                2,
                self.request_id,
            )

        self.assertEqual(task_id, self.task_id)
        call = post.call_args
        self.assertEqual(call.args[0], volc_lark._SUBMIT)
        self.assertEqual(
            call.kwargs["headers"]["X-Api-Request-Id"],
            self.request_id,
        )
        self.assertEqual(
            call.kwargs["json"]["Input"]["Offline"]["FileURL"],
            self.file_url,
        )
        self.assertEqual(
            call.kwargs["json"]["Params"]["AudioTranscriptionParams"][
                "NumberOfSpeaker"
            ],
            2,
        )

    def test_resume_poll_uses_persisted_ids_without_submit(self):
        with (
            patch.object(
                volc_lark.requests,
                "post",
                return_value=self._success_query_response(),
            ) as post,
            patch.object(
                volc_lark.requests,
                "get",
                return_value=self._transcript_response(),
            ) as get,
        ):
            sentences = volc_lark.poll_lark_result(
                self.request_id,
                self.task_id,
            )

        self.assertEqual(sentences[0]["content"], "hello")
        post.assert_called_once()
        self.assertEqual(post.call_args.args[0], volc_lark._QUERY)
        self.assertEqual(post.call_args.kwargs["json"], {"TaskID": self.task_id})
        self.assertEqual(
            post.call_args.kwargs["headers"]["X-Api-Request-Id"],
            self.request_id,
        )
        get.assert_called_once_with(self.transcript_url, timeout=120)

    def test_compatibility_wrapper_checkpoints_task_before_first_poll(self):
        events = []

        def post(url, **kwargs):
            events.append(
                (
                    "submit" if url == volc_lark._SUBMIT else "poll",
                    kwargs["headers"]["X-Api-Request-Id"],
                )
            )
            if url == volc_lark._SUBMIT:
                return self._submit_response()
            return self._success_query_response()

        def checkpoint(request_id, task_id):
            events.append(("checkpoint", request_id, task_id))

        with (
            patch.object(volc_lark.uuid, "uuid4", return_value=self.request_id),
            patch.object(volc_lark.requests, "post", side_effect=post),
            patch.object(
                volc_lark.requests,
                "get",
                return_value=self._transcript_response(),
            ),
        ):
            volc_lark._submit_poll(
                self.file_url,
                0,
                task_checkpoint=checkpoint,
            )

        self.assertEqual(
            events,
            [
                ("submit", self.request_id),
                ("checkpoint", self.request_id, self.task_id),
                ("poll", self.request_id),
            ],
        )

    def test_submit_transport_failures_raise_redacted_ambiguity(self):
        leaked_url = self.file_url
        leaked_key = "super-secret-api-key"
        for transport_error in (
            requests.Timeout(f"timeout posting {leaked_url} with {leaked_key}"),
            requests.ConnectionError(
                f"connection reset posting {leaked_url} with {leaked_key}"
            ),
            requests.exceptions.ChunkedEncodingError(
                f"truncated response from {leaked_url} with {leaked_key}"
            ),
        ):
            with self.subTest(error=type(transport_error).__name__):
                with (
                    patch.dict("os.environ", {"VOLC_API_KEY": leaked_key}),
                    patch.object(
                        volc_lark.requests,
                        "post",
                        side_effect=transport_error,
                    ),
                    self.assertRaises(volc_lark.LarkSubmitAmbiguousError) as raised,
                ):
                    volc_lark.submit_lark_job(
                        leaked_url,
                        0,
                        self.request_id,
                    )

                message = str(raised.exception)
                self.assertIn("ambiguous", message)
                self.assertNotIn(leaked_url, message)
                self.assertNotIn("signature=private", message)
                self.assertNotIn(leaked_key, message)
                self.assertIsNone(raised.exception.__cause__)

    def test_submit_rejects_malformed_responses(self):
        malformed = (
            FakeResponse(
                headers={"X-Api-Status-Code": "20000000"},
                json_error=ValueError("not json"),
            ),
            FakeResponse(
                headers={"X-Api-Status-Code": "20000000"},
                payload=[],
            ),
            FakeResponse(
                headers={"X-Api-Status-Code": "20000000"},
                payload={"Data": []},
            ),
            FakeResponse(
                headers={"X-Api-Status-Code": "20000000"},
                payload={"Data": {"TaskID": ""}},
            ),
        )

        for response in malformed:
            with self.subTest(payload=response._payload):
                with (
                    patch.object(
                        volc_lark.requests,
                        "post",
                        return_value=response,
                    ),
                    self.assertRaises(volc_lark.LarkSubmitAmbiguousError),
                ):
                    volc_lark.submit_lark_job(
                        self.file_url,
                        0,
                        self.request_id,
                    )

    def test_submit_http_5xx_is_ambiguous_but_explicit_rejection_is_not(self):
        with (
            patch.object(
                volc_lark.requests,
                "post",
                return_value=FakeResponse(status_code=503),
            ),
            self.assertRaises(volc_lark.LarkSubmitAmbiguousError),
        ):
            volc_lark.submit_lark_job(self.file_url, 0, self.request_id)

        with (
            patch.object(
                volc_lark.requests,
                "post",
                return_value=FakeResponse(
                    status_code=400,
                    headers={"X-Api-Status-Code": "40000001"},
                ),
            ),
            self.assertRaises(volc_lark.LarkSubmitRejectedError),
        ):
            volc_lark.submit_lark_job(self.file_url, 0, self.request_id)

    def test_provider_error_messages_never_echo_response_bodies(self):
        secret_body = (
            "https://audio.example.test/object.mp3?X-Tos-Signature=secret "
            "private transcript text"
        )
        cases = (
            (
                lambda: volc_lark.submit_lark_job(self.file_url, 0, self.request_id),
                patch.object(
                    volc_lark.requests,
                    "post",
                    return_value=FakeResponse(
                        headers={"X-Api-Status-Code": "50000000"},
                        text=secret_body,
                    ),
                ),
            ),
            (
                lambda: volc_lark.poll_lark_result(self.request_id, self.task_id),
                patch.object(
                    volc_lark.requests,
                    "post",
                    return_value=FakeResponse(status_code=500, text=secret_body),
                ),
            ),
        )
        for operation, mocked_request in cases:
            with self.subTest(operation=operation), mocked_request:
                with self.assertRaises(volc_lark.LarkError) as raised:
                    operation()
                self.assertNotIn(secret_body, str(raised.exception))
                self.assertNotIn("X-Tos-Signature", str(raised.exception))

    def test_poll_rejects_malformed_query_and_transcript_responses(self):
        malformed_queries = (
            FakeResponse(json_error=ValueError("not json")),
            FakeResponse(payload=[]),
            FakeResponse(payload={"Data": []}),
            FakeResponse(payload={"Data": {"Status": "success", "Result": []}}),
        )
        for response in malformed_queries:
            with self.subTest(payload=response._payload):
                with (
                    patch.object(
                        volc_lark.requests,
                        "post",
                        return_value=response,
                    ),
                    self.assertRaises(volc_lark.LarkError),
                ):
                    volc_lark.poll_lark_result(self.request_id, self.task_id)

        malformed_transcripts = (
            FakeResponse(json_error=ValueError("not json")),
            FakeResponse(payload={"sentences": []}),
            FakeResponse(payload=["not-an-object"]),
        )
        for response in malformed_transcripts:
            with self.subTest(payload=response._payload):
                with (
                    patch.object(
                        volc_lark.requests,
                        "post",
                        return_value=self._success_query_response(),
                    ),
                    patch.object(
                        volc_lark.requests,
                        "get",
                        return_value=response,
                    ),
                    self.assertRaises(volc_lark.LarkError),
                ):
                    volc_lark.poll_lark_result(self.request_id, self.task_id)

    def test_durable_functions_require_persisted_identifiers(self):
        with patch.object(volc_lark.requests, "post") as post:
            with self.assertRaises(volc_lark.LarkError):
                volc_lark.submit_lark_job(self.file_url, 0, "")
            with self.assertRaises(volc_lark.LarkError):
                volc_lark.poll_lark_result(self.request_id, "")
        post.assert_not_called()


if __name__ == "__main__":
    unittest.main()
