# Explicit build, because Railway's static-site provider otherwise detects
# index.html and serves the source tree with Caddy instead of the built output.
FROM node:22-alpine AS build
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM node:22-alpine
WORKDIR /app
ENV NODE_ENV=production
COPY --from=build /app/dist ./dist
COPY --from=build /app/server.mjs ./server.mjs
# The backend address is read at runtime from API_BASE_URL, so pointing this at
# a different backend is a variable change, not a rebuild.
CMD ["node", "server.mjs"]
