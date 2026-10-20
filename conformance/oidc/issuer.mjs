// SPDX-License-Identifier: AGPL-3.0-or-later
//
// An independent OpenID Connect issuer for korpis-idp-oidc's tests: panva's oidc-provider, an
// OpenID Certified implementation, serving codes with PKCE, ID tokens, and rotating refresh
// tokens whose reuse revokes the grant (RFC 9700 §4.14.2). Only who signs in is the test's: there
// is no login form, and an interaction signs in the account the people file names as `next` and
// grants what was asked. The people file is read at every use, so a test removes someone from a
// group, or from the issuer, by writing it.
//
//   PORT       the loopback port to listen on
//   PEOPLE     a JSON file: { "next": "<sub>", "people": { "<sub>": ["<group>", ...] } }
//   CLIENT_ID, CLIENT_SECRET, REDIRECT_URI   the one client
//   ALG        the ID token's algorithm: RS256, ES256, or EdDSA

import { generateKeyPairSync } from "node:crypto";
import { readFileSync } from "node:fs";
import Provider from "oidc-provider";

const env = (k) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} is not set`);
  return v;
};
const people = () => JSON.parse(readFileSync(env("PEOPLE"), "utf8"));
const alg = process.env.ALG ?? "ES256";
const pair =
  alg === "RS256"
    ? generateKeyPairSync("rsa", { modulusLength: 2048 })
    : alg === "EdDSA"
      ? generateKeyPairSync("ed25519")
      : generateKeyPairSync("ec", { namedCurve: "P-256" });
const jwk = { ...pair.privateKey.export({ format: "jwk" }), kid: "k1", alg, use: "sig" };
const issuer = `http://127.0.0.1:${env("PORT")}`;

const provider = new Provider(issuer, {
  clients: [
    {
      client_id: env("CLIENT_ID"),
      client_secret: env("CLIENT_SECRET"),
      redirect_uris: [env("REDIRECT_URI")],
      grant_types: ["authorization_code", "refresh_token"],
      response_types: ["code"],
      token_endpoint_auth_method: "client_secret_basic",
      id_token_signed_response_alg: alg,
    },
  ],
  jwks: { keys: [jwk] },
  scopes: ["openid", "offline_access", "groups"],
  claims: { openid: ["sub"], groups: ["groups"] },
  // Groups in the ID token, as korpis-idp-oidc reads them, not only at the userinfo endpoint.
  conformIdTokenClaims: false,
  rotateRefreshToken: true,
  pkce: { required: () => true },
  features: { devInteractions: { enabled: false } },
  interactions: { url: (_ctx, interaction) => `/interaction/${interaction.uid}` },
  async findAccount(_ctx, sub) {
    const groups = people().people[sub];
    if (!groups) return undefined;
    return { accountId: sub, claims: async () => ({ sub, groups }) };
  },
});

provider.use(async (ctx, next) => {
  const match = /^\/interaction\/([^/]+)$/.exec(ctx.path);
  if (!match) return next();
  const details = await provider.interactionDetails(ctx.req, ctx.res);
  let result;
  if (details.prompt.name === "login") {
    result = { login: { accountId: people().next } };
  } else {
    const grant = new provider.Grant({
      accountId: details.session.accountId,
      clientId: details.params.client_id,
    });
    grant.addOIDCScope(details.params.scope);
    result = { consent: { grantId: await grant.save() } };
  }
  await provider.interactionFinished(ctx.req, ctx.res, result, {
    mergeWithLastSubmission: false,
  });
});

provider.listen(Number(env("PORT")), "127.0.0.1", () => {
  process.stdout.write(`listening ${issuer}\n`);
});
