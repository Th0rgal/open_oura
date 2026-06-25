#ifndef OURA_FFI_H
#define OURA_FFI_H

#include <stdint.h>
#include <stddef.h>

/* Encrypt a ring auth nonce (AES-128/ECB/PKCS7) into `out` (16 bytes).
 * `key` must be 16 bytes; `nonce` typically 15. Returns 0 on success. */
int32_t oura_encrypt_nonce(const uint8_t *key, size_t key_len,
                           const uint8_t *nonce, size_t nonce_len,
                           uint8_t *out);

/* Decode an event body to a JSON string (or NULL). Free with oura_string_free. */
char *oura_decode_event(uint8_t tag, const uint8_t *body, size_t body_len);

/* Event name for a tag (owned string). Free with oura_string_free. */
char *oura_event_name(uint8_t tag);

/* Release a string returned by this library. */
void oura_string_free(char *ptr);

#endif /* OURA_FFI_H */
