// The server speaks the WebAuthn JSON shapes; newer browsers parse the options
// themselves, and the fallback decodes the base64url fields by hand. Responses
// are assembled field by field rather than through `toJSON()`, which Safari and
// older Chrome do not have.

export type CreationChallenge = { publicKey: PublicKeyCredentialCreationOptionsJSON }
export type RequestChallenge = { publicKey: PublicKeyCredentialRequestOptionsJSON }

export type RegistrationJSON = {
  id: string
  rawId: string
  type: string
  response: { attestationObject: string; clientDataJSON: string; transports?: string[] }
}

export type AssertionJSON = {
  id: string
  rawId: string
  type: string
  response: {
    authenticatorData: string
    clientDataJSON: string
    signature: string
    userHandle: string | null
  }
}

export function webauthnSupported(): boolean {
  return typeof window !== 'undefined' && 'PublicKeyCredential' in window
}

function decode(text: string): Uint8Array {
  const padded = text.replace(/-/g, '+').replace(/_/g, '/')
  const raw = atob(padded.padEnd(Math.ceil(padded.length / 4) * 4, '='))
  return Uint8Array.from(raw, (c) => c.charCodeAt(0))
}

function encode(buffer: ArrayBuffer): string {
  let raw = ''
  for (const byte of new Uint8Array(buffer)) raw += String.fromCharCode(byte)
  return btoa(raw).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

const parses = (name: 'parseCreationOptionsFromJSON' | 'parseRequestOptionsFromJSON') =>
  webauthnSupported() && typeof PublicKeyCredential[name] === 'function'

function creationOptions(json: PublicKeyCredentialCreationOptionsJSON) {
  if (parses('parseCreationOptionsFromJSON')) return PublicKeyCredential.parseCreationOptionsFromJSON(json)
  return {
    ...json,
    challenge: decode(json.challenge),
    user: { ...json.user, id: decode(json.user.id) },
    excludeCredentials: json.excludeCredentials?.map((c) => ({
      ...c,
      id: decode(c.id),
      transports: c.transports as AuthenticatorTransport[] | undefined,
    })),
  } as PublicKeyCredentialCreationOptions
}

function requestOptions(json: PublicKeyCredentialRequestOptionsJSON) {
  if (parses('parseRequestOptionsFromJSON')) return PublicKeyCredential.parseRequestOptionsFromJSON(json)
  return {
    ...json,
    challenge: decode(json.challenge),
    allowCredentials: json.allowCredentials?.map((c) => ({
      ...c,
      id: decode(c.id),
      transports: c.transports as AuthenticatorTransport[] | undefined,
    })),
  } as PublicKeyCredentialRequestOptions
}

export async function createCredential(challenge: CreationChallenge): Promise<RegistrationJSON> {
  const credential = await navigator.credentials.create({
    publicKey: creationOptions(challenge.publicKey),
  })
  if (!(credential instanceof PublicKeyCredential)) throw new Error('no credential was created')
  const response = credential.response as AuthenticatorAttestationResponse
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: encode(response.attestationObject),
      clientDataJSON: encode(response.clientDataJSON),
      transports: response.getTransports?.(),
    },
  }
}

export async function signChallenge(challenge: RequestChallenge): Promise<AssertionJSON> {
  const credential = await navigator.credentials.get({
    publicKey: requestOptions(challenge.publicKey),
  })
  if (!(credential instanceof PublicKeyCredential)) throw new Error('no passkey answered')
  const response = credential.response as AuthenticatorAssertionResponse
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: encode(response.authenticatorData),
      clientDataJSON: encode(response.clientDataJSON),
      signature: encode(response.signature),
      userHandle: response.userHandle ? encode(response.userHandle) : null,
    },
  }
}
