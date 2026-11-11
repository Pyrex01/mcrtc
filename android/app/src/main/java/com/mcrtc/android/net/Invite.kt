// Invite and reply strings: the only signaling mcrtc has. A port of `src/invite.rs`.
//
// A payload is JSON, deflated, base64url encoded (no padding) and prefixed with `mcrtc1:`, so
// it stays on one line and survives a trip through Discord or WhatsApp.

package com.mcrtc.android.net

import java.io.ByteArrayOutputStream
import java.util.Base64
import java.util.zip.DataFormatException
import java.util.zip.Deflater
import java.util.zip.Inflater
import org.json.JSONException
import org.json.JSONObject

const val PREFIX = "mcrtc1:"
const val VERSION = 1
const val DEFAULT_TTL_MS = 10 * 60 * 1000L

/** Far above any real SDP. Invites come from other people, so a small deflate bomb must not be
 *  able to allocate unbounded memory. */
private const val MAX_DECODED_BYTES = 256 * 1024

class MessageException(message: String) : Exception(message)

fun fail(message: String): Nothing = throw MessageException(message)

/** Which game is being tunneled. The Android app carries Java Edition only. */
enum class Edition(val wire: String, val title: String) {
    Java("java", "Java Edition"),
    Bedrock("bedrock", "Bedrock Edition");

    companion object {
        fun fromWire(text: String): Edition =
            entries.firstOrNull { it.wire == text } ?: fail("unknown edition `$text`")
    }
}

enum class PayloadType(val wire: String, val noun: String, val article: String) {
    Offer("offer", "invite", "an"),
    Answer("answer", "reply", "a"),
}

fun unixNow(): Long = System.currentTimeMillis() / 1000

data class Payload(
    val v: Int,
    val kind: PayloadType,
    val sdp: String,
    val expires: Long,
    val edition: Edition,
) {
    /** The compact single-line form. */
    fun encode(): String {
        val json = JSONObject()
            .put("v", v)
            .put("type", kind.wire)
            .put("sdp", sdp)
            .put("expires", expires)
        // Left out for Java, so those strings stay as short as they always were.
        if (edition != Edition.Java) json.put("edition", edition.wire)

        val deflater = Deflater(Deflater.BEST_COMPRESSION, true)
        deflater.setInput(json.toString().toByteArray())
        deflater.finish()
        val compressed = ByteArrayOutputStream()
        val buffer = ByteArray(4096)
        while (!deflater.finished()) {
            compressed.write(buffer, 0, deflater.deflate(buffer))
        }
        deflater.end()
        return PREFIX + Base64.getUrlEncoder().withoutPadding().encodeToString(compressed.toByteArray())
    }

    /** Checks the version, the direction and the expiry against `now` (unix seconds). */
    fun validate(expected: PayloadType, now: Long) {
        if (v != VERSION) {
            fail("unsupported version $v (this mcrtc speaks version $VERSION); you and your friend need the same mcrtc version")
        }
        if (kind != expected) {
            fail("expected ${expected.article} ${expected.noun} but this is ${kind.article} ${kind.noun}")
        }
        if (expires <= now) {
            fail("this ${kind.noun} expired ${now - expires} seconds ago; ask for a new one")
        }
        if (sdp.isBlank()) fail("the ${kind.noun} has an empty SDP")
    }

    /** A Java game cannot use a Bedrock tunnel or the other way around, so say so up front
     *  instead of leaving Minecraft to time out. */
    fun checkEdition(expected: Edition) {
        if (edition != expected) {
            fail("this ${kind.noun} is for ${edition.title}, but the Android app only carries ${expected.title}")
        }
    }

    companion object {
        fun create(kind: PayloadType, sdp: String, edition: Edition) =
            Payload(VERSION, kind, sdp, unixNow() + DEFAULT_TTL_MS / 1000, edition)

        /** Parses either form, whichever was pasted, without judging its contents. */
        fun decode(input: String): Payload {
            // Chat apps and terminals like to wrap long lines.
            val compact = input.filterNot { it.isWhitespace() }
            if (compact.isEmpty()) fail("nothing was pasted")
            if (compact.startsWith('{')) {
                return fromJson(input) ?: fail("this is not valid mcrtc JSON")
            }
            if (!compact.startsWith(PREFIX)) {
                val prefix = compact.substringBefore(':', "")
                if (prefix.startsWith("mcrtc")) {
                    fail("unsupported format `$prefix:`; you and your friend need the same mcrtc version")
                }
                fail("this does not look like an mcrtc string (it should start with `$PREFIX`)")
            }

            val compressed = try {
                Base64.getUrlDecoder().decode(compact.removePrefix(PREFIX))
            } catch (_: IllegalArgumentException) {
                fail("the string is corrupted or was cut off while copying (bad base64)")
            }
            val json = inflate(compressed)
            return fromJson(String(json)) ?: fail("the string does not contain valid mcrtc data")
        }

        /** Decodes and validates a pasted string against the current time. */
        fun parse(input: String, expected: PayloadType): Payload =
            decode(input).also { it.validate(expected, unixNow()) }

        private fun inflate(compressed: ByteArray): ByteArray {
            val corrupted = "the string is corrupted or was cut off while copying (bad deflate data)"
            val inflater = Inflater(true)
            inflater.setInput(compressed)
            val output = ByteArrayOutputStream()
            val buffer = ByteArray(4096)
            try {
                while (!inflater.finished()) {
                    val read = inflater.inflate(buffer)
                    if (read == 0 && (inflater.needsInput() || inflater.needsDictionary())) fail(corrupted)
                    output.write(buffer, 0, read)
                    if (output.size() > MAX_DECODED_BYTES) {
                        fail("the string decodes to more than $MAX_DECODED_BYTES bytes, refusing it")
                    }
                }
            } catch (_: DataFormatException) {
                fail(corrupted)
            } finally {
                inflater.end()
            }
            return output.toByteArray()
        }

        private fun fromJson(text: String): Payload? = try {
            val json = JSONObject(text)
            val kind = when (json.getString("type")) {
                "offer" -> PayloadType.Offer
                "answer" -> PayloadType.Answer
                else -> null
            }
            kind?.let {
                Payload(
                    v = json.getInt("v"),
                    kind = it,
                    sdp = json.getString("sdp"),
                    expires = json.getLong("expires"),
                    edition = Edition.fromWire(json.optString("edition", "java")),
                )
            }
        } catch (_: JSONException) {
            null
        } catch (_: MessageException) {
            null
        }
    }
}

/**
 * Shortens an SDP for pasting by dropping candidates that add nothing: the RTCP component, which
 * a data channel never uses, repeats of an address, and wildcards nobody can reach. This only
 * changes what the other side is told, not the local ICE state.
 */
fun trimCandidates(sdp: String): String {
    val seen = HashSet<List<String>>()
    val trimmed = StringBuilder(sdp.length)
    var start = 0
    while (start < sdp.length) {
        val end = sdp.indexOf('\n', start).let { if (it < 0) sdp.length else it + 1 }
        val line = sdp.substring(start, end)
        start = end
        if (line.startsWith("a=candidate:")) {
            val fields = line.trim().split(Regex("\\s+"))
            // a=candidate:<foundation> <component> <protocol> <priority> <address> <port> typ <type>
            if (fields.size >= 8) {
                val (component, protocol, address, port, kind) =
                    listOf(fields[1], fields[2], fields[4], fields[5], fields[7])
                val wildcard = address == "::" || address == "0.0.0.0"
                if (component != "1" || wildcard || !seen.add(listOf(protocol, address, port, kind))) {
                    continue
                }
            }
        }
        trimmed.append(line)
    }
    return trimmed.toString()
}
