import {
  bearerToken,
  copyHeaders,
  errorResponse,
  optionalEnv,
  readJson,
  requireEnv,
  upstreamUrl,
} from "../../_lib/http";

export default {
  async fetch(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return new Response(null, { status: 405 });
    }

    try {
      const chatUrl =
        optionalEnv("CODEUS_CHAT_COMPLETIONS_URL") ??
        upstreamUrl(requireEnv("CODEUS_CHAT_BASE"), "chat/completions");
      const serverToken = optionalEnv("CODEUS_CHAT_API_KEY");
      const userToken = bearerToken(request);
      const body = await readJson(request);
      const headers = new Headers({
        "content-type": "application/json",
      });

      if (serverToken) {
        headers.set("authorization", `Bearer ${serverToken}`);
        if (userToken) {
          headers.set("x-codeus-session-token", userToken);
        }
      } else if (userToken) {
        headers.set("authorization", `Bearer ${userToken}`);
      }

      const response = await fetch(chatUrl, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
      });

      const responseHeaders = copyHeaders(response.headers);
      responseHeaders.set("cache-control", "no-store");

      return new Response(response.body, {
        status: response.status,
        headers: responseHeaders,
      });
    } catch (error) {
      return errorResponse(error);
    }
  },
};
