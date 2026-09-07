use crate::auth::SharedAuthProvider;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::telemetry::run_with_request_telemetry;
use crate::telemetry::run_with_request_telemetry_preserving_status;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RawRetryOutcome;
use codex_client::Request;
use codex_client::RequestBody;
use codex_client::RequestTelemetry;
use codex_client::Response;
use codex_client::StreamResponse;
use codex_client::TransportError;
use http::HeaderMap;
use http::Method;
use serde_json::Value;
use std::sync::Arc;
use tracing::instrument;

pub(crate) struct EndpointSession<T: HttpTransport> {
    transport: T,
    provider: Provider,
    auth: SharedAuthProvider,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
}

impl<T: HttpTransport> EndpointSession<T> {
    pub(crate) fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            transport,
            provider,
            auth,
            request_telemetry: None,
        }
    }

    pub(crate) fn with_request_telemetry(
        mut self,
        request: Option<Arc<dyn RequestTelemetry>>,
    ) -> Self {
        self.request_telemetry = request;
        self
    }

    pub(crate) fn provider(&self) -> &Provider {
        &self.provider
    }

    fn make_request(
        &self,
        method: &Method,
        path: &str,
        extra_headers: &HeaderMap,
        body: Option<&RequestBody>,
    ) -> Request {
        let mut req = self.provider.build_request(method.clone(), path);
        req.headers.extend(extra_headers.clone());
        if let Some(body) = body {
            req.body = Some(body.clone());
        }
        req
    }

    pub(crate) async fn execute(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<Value>,
    ) -> Result<Response, ApiError> {
        self.execute_with(method, path, extra_headers, body, |_| {})
            .await
    }

    #[instrument(
        name = "endpoint_session.execute_with",
        level = "info",
        skip_all,
        fields(http.method = %method, api.path = path)
    )]
    pub(crate) async fn execute_with<C>(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<Value>,
        configure: C,
    ) -> Result<Response, ApiError>
    where
        C: Fn(&mut Request),
    {
        let body = body.map(RequestBody::Json);
        let make_request = || {
            let mut req = self.make_request(&method, path, &extra_headers, body.as_ref());
            configure(&mut req);
            req
        };

        let response = run_with_request_telemetry(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            make_request,
            |req| {
                let auth = self.auth.clone();
                let transport = &self.transport;
                async move {
                    let req = auth.apply_auth(req).await.map_err(TransportError::from)?;
                    transport.execute(req).await
                }
            },
        )
        .await?;

        Ok(response)
    }

    #[instrument(
        name = "endpoint_session.stream_encoded_json_with",
        level = "info",
        skip_all,
        fields(http.method = %method, api.path = path)
    )]
    pub(crate) async fn stream_encoded_json_with<C>(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<EncodedJsonBody>,
        configure: C,
    ) -> Result<StreamResponse, ApiError>
    where
        C: Fn(&mut Request),
    {
        let body = body.map(RequestBody::EncodedJson);
        let mut request = self.make_request(&method, path, &extra_headers, body.as_ref());
        configure(&mut request);
        let request = request.into_prepared().map_err(TransportError::Build)?;
        let make_request = || request.clone();

        let stream = run_with_request_telemetry(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            make_request,
            |req| {
                let auth = self.auth.clone();
                let transport = &self.transport;
                async move {
                    let req = auth.apply_auth(req).await.map_err(TransportError::from)?;
                    transport.stream(req).await
                }
            },
        )
        .await?;

        Ok(stream)
    }

    /// raw unary 形态（transportd R6）：任何 status 的响应原样返回
    /// （terminal non-2xx 不映射为 `ApiError`），并携带官方 retry 后的
    /// 真实 attempt 次数。重试语义与 [`Self::execute_with`] 一致。
    #[instrument(
        name = "endpoint_session.execute_raw_with",
        level = "info",
        skip_all,
        fields(http.method = %method, api.path = path)
    )]
    pub(crate) async fn execute_raw_with<C>(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<Value>,
        configure: C,
    ) -> Result<RawRetryOutcome<Response>, ApiError>
    where
        C: Fn(&mut Request),
    {
        let body = body.map(RequestBody::Json);
        let make_request = || {
            let mut req = self.make_request(&method, path, &extra_headers, body.as_ref());
            configure(&mut req);
            req
        };

        run_with_request_telemetry_preserving_status(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            make_request,
            |req| {
                let auth = self.auth.clone();
                let transport = &self.transport;
                async move {
                    let req = auth.apply_auth(req).await.map_err(TransportError::from)?;
                    transport.execute_preserving_status(req).await
                }
            },
        )
        .await
        .map_err(ApiError::from)
    }

    /// raw 流式形态（transportd R6）：terminal non-2xx 以 `Ok` 返回且
    /// body 可逐块读取；attempt 次数真实。重试语义与
    /// [`Self::stream_encoded_json_with`] 一致。
    #[instrument(
        name = "endpoint_session.stream_raw_with",
        level = "info",
        skip_all,
        fields(http.method = %method, api.path = path)
    )]
    pub(crate) async fn stream_raw_with<C>(
        &self,
        method: Method,
        path: &str,
        extra_headers: HeaderMap,
        body: Option<EncodedJsonBody>,
        configure: C,
    ) -> Result<RawRetryOutcome<StreamResponse>, ApiError>
    where
        C: Fn(&mut Request),
    {
        let body = body.map(RequestBody::EncodedJson);
        let mut request = self.make_request(&method, path, &extra_headers, body.as_ref());
        configure(&mut request);
        let request = request.into_prepared().map_err(TransportError::Build)?;
        let make_request = || request.clone();

        run_with_request_telemetry_preserving_status(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            make_request,
            |req| {
                let auth = self.auth.clone();
                let transport = &self.transport;
                async move {
                    let req = auth.apply_auth(req).await.map_err(TransportError::from)?;
                    transport.stream_preserving_status(req).await
                }
            },
        )
        .await
        .map_err(ApiError::from)
    }
}
