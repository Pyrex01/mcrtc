package com.mcrtc.android.net

import java.util.Base64
import java.util.zip.Deflater
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class ProtocolTest {
    private val sdp = "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\na=fingerprint:sha-256 AB:CD\r\n".repeat(20)

    private fun sample(kind: PayloadType) = Payload(VERSION, kind, sdp, 2_000, Edition.Java)

    private fun error(input: String): String = try {
        Payload.decode(input)
        fail("$input decoded")
        ""
    } catch (error: MessageException) {
        error.message!!
    }

    /** Made by serde_json, raw deflate and unpadded base64url, as `src/invite.rs` makes them. */
    private val desktopInvite = "mcrtc1:q1YqU7Iy1FEqqSxIVbJSyk9LSy1S0lEqTikA8spsDWKKYvLybXUVDBWMFDz9FDwDTBQM9MAQJJNom5aZl55aVFCUmVdiVZyRqGtkaqbg6GTl7AKSHtU_qn9U_6j-oaofWBCmVhRkFqUWK1kZGRgY1AIA"
    private val desktopBedrockInvite = "mcrtc1:q1YqU7Iy1FEqqSxIVbJSyk9LSy1S0lEqTikA8spsDWKKYvKA_NSKgsyi1GIlKyMDAwMgNyWzJDM_D6gkKTWlKD85W6kWAA"

    @Test
    fun readsWhatTheDesktopWrites() {
        assertEquals(sample(PayloadType.Offer), Payload.decode(desktopInvite))
        val bedrock = Payload.decode(desktopBedrockInvite)
        assertEquals(Edition.Bedrock, bedrock.edition)
        val message = try {
            bedrock.checkEdition(Edition.Java)
            ""
        } catch (error: MessageException) {
            error.message!!
        }
        assertTrue(message, "invite is for Bedrock Edition" in message)
    }

    @Test
    fun compactRoundTrip() {
        val payload = sample(PayloadType.Offer)
        val encoded = payload.encode()
        assertTrue(encoded.startsWith(PREFIX))
        assertFalse(encoded.any { it.isWhitespace() || it in "+/=" })
        assertEquals(payload, Payload.decode(encoded))
    }

    @Test
    fun javaStringsCarryNoEdition() {
        val encoded = sample(PayloadType.Answer).encode()
        val json = String(java.util.zip.InflaterInputStream(
            Base64.getUrlDecoder().decode(encoded.removePrefix(PREFIX)).inputStream(),
            java.util.zip.Inflater(true),
        ).readBytes())
        assertFalse(json, "edition" in json)
        assertTrue(json, "\"type\":\"answer\"" in json)
    }

    @Test
    fun whitespaceFromChatAppsIsIgnored() {
        val encoded = sample(PayloadType.Offer).encode()
        val wrapped = "  ${encoded.take(40)}\r\n  ${encoded.drop(40)}\n"
        assertEquals(sample(PayloadType.Offer), Payload.decode(wrapped))
    }

    @Test
    fun validatesVersionDirectionAndExpiry() {
        val offer = sample(PayloadType.Offer)
        offer.validate(PayloadType.Offer, 1_999)
        for ((payload, expected, text) in listOf(
            Triple(offer, PayloadType.Offer, "expired"),
            Triple(offer.copy(v = 2), PayloadType.Offer, "unsupported version 2"),
            Triple(offer, PayloadType.Answer, "expected a reply"),
        )) {
            val message = try {
                payload.validate(expected, 2_000)
                ""
            } catch (error: MessageException) {
                error.message!!
            }
            assertTrue(message, text in message)
        }
    }

    @Test
    fun rejectsBadInput() {
        for ((input, expected) in listOf(
            "" to "nothing was pasted",
            "   \n " to "nothing was pasted",
            "hello friend" to "does not look like",
            "mcrtc2:abcd" to "unsupported format",
            "mcrtc1:not*base64!" to "bad base64",
            "mcrtc1:AAAA" to "bad deflate",
            "{\"v\": 1}" to "not valid mcrtc JSON",
        )) {
            assertTrue("$input gave ${error(input)}", expected in error(input))
        }
        assertTrue(error(sample(PayloadType.Offer).encode().dropLast(30)).isNotEmpty())
    }

    @Test
    fun rejectsDecompressionBombs() {
        val deflater = Deflater(Deflater.BEST_COMPRESSION, true)
        deflater.setInput(ByteArray(2 * 1024 * 1024) { ' '.code.toByte() })
        deflater.finish()
        val buffer = ByteArray(1 shl 16)
        val size = deflater.deflate(buffer)
        val bomb = PREFIX + Base64.getUrlEncoder().withoutPadding().encodeToString(buffer.copyOf(size))
        assertTrue(bomb.length < 8_192)
        assertTrue("refusing" in error(bomb))
    }

    @Test
    fun trimmingDropsRepeatsAndWildcardsOnly() {
        val sdp = "v=0\r\n" +
            "a=candidate:1 1 udp 9 10.0.0.1 5000 typ host\r\n" +
            "a=candidate:2 1 udp 8 10.0.0.1 5000 typ host\r\n" +
            "a=candidate:3 1 udp 7 10.0.0.1 5001 typ host\r\n" +
            "a=candidate:4 1 udp 6 203.0.113.9 5000 typ srflx raddr 0.0.0.0 rport 0\r\n" +
            "a=candidate:5 1 udp 5 203.0.113.9 5000 typ srflx raddr 0.0.0.0 rport 0\r\n" +
            "a=candidate:6 1 udp 4 :: 5002 typ host\r\n" +
            "a=candidate:7 1 udp 3 0.0.0.0 5003 typ host\r\n" +
            "a=candidate:8 2 udp 2 10.0.0.2 5004 typ host\r\n" +
            "a=mid:0\r\n"
        val trimmed = trimCandidates(sdp)
        assertEquals(3, Regex("a=candidate").findAll(trimmed).count())
        for (kept in listOf("a=candidate:1 ", "a=candidate:3 ", "a=candidate:4 ", "v=0\r\n", "a=mid:0\r\n")) {
            assertTrue(kept, kept in trimmed)
        }
    }

    @Test
    fun streamIdsMatchTheDesktopAndFitLibwebrtc() {
        assertEquals(2, streamId(0))
        assertEquals(4, streamId(1))
        assertEquals(streamId(0), streamId(500))
        assertTrue((0L until 1_000L).all { streamId(it) in 2 until 1024 })
        assertEquals("mc-7", channelLabel(7))
    }

    @Test
    fun controlAnswersResolveOnlyTheirRequest() {
        val opened = mutableListOf<Pair<Long, Transport>>()
        val host = Role.Host { id, transport -> opened += id to transport }
        host.handle("open 3")
        host.handle("open-datagrams 6")
        host.handle("ready 4")
        host.handle("nonsense")
        assertEquals(listOf(3L to Transport.Stream, 6L to Transport.Datagrams), opened)
    }

    @Test
    fun addressesSplitIntoHostAndPort() {
        assertEquals("127.0.0.1" to 25565, splitAddress("127.0.0.1:25565", 1))
        assertEquals("example.com" to 25565, splitAddress(" example.com ", 25565))
        assertEquals("::1" to 25570, splitAddress("[::1]:25570", 1))
    }
}
