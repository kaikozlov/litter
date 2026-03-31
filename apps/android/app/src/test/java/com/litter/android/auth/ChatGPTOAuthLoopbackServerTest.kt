package com.litter.android.auth

import android.net.Uri
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatGPTOAuthLoopbackServerTest {
    @Test
    fun requestTargetFromLine_parsesGetRequests() {
        assertEquals(
            "/auth/callback?code=abc&state=xyz",
            ChatGPTOAuthLoopbackServer.requestTargetFromLine(
                "GET /auth/callback?code=abc&state=xyz HTTP/1.1",
            ),
        )
    }

    @Test
    fun callbackUriForRequest_reusesRedirectOrigin() {
        val callbackUri = ChatGPTOAuthLoopbackServer.callbackUriForRequest(
            redirectUri = Uri.parse("http://localhost:1455/auth/callback"),
            requestTarget = "/auth/callback?code=abc&state=xyz",
        )

        assertEquals("http", callbackUri.scheme)
        assertEquals("localhost", callbackUri.host)
        assertEquals(1455, callbackUri.port)
        assertEquals("/auth/callback", callbackUri.path)
        assertEquals("abc", callbackUri.getQueryParameter("code"))
        assertEquals("xyz", callbackUri.getQueryParameter("state"))
    }

    @Test
    fun successHtml_mentionsReturnToApp() {
        val html = ChatGPTOAuthLoopbackServer.successHtml("litterauth://chatgpt-auth-complete")
        assertTrue(html.contains("Returning to Litter"))
        assertTrue(html.contains("litterauth://chatgpt-auth-complete"))
    }
}
