use super::*;

impl<S: MultiplexTransport> MultiplexedClient<S> {
    pub(super) fn call_op(&self, op: Op) -> Result<Completion> {
        let expected_tag = op.tag;
        let pending = self.submit_op(op)?;
        let _guard = self.observe_call(expected_tag);
        match pending.wait()? {
            ClientResponse::Completion { tag, completion } if tag == expected_tag => Ok(completion),
            ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
            other => Err(Error::from(format!(
                "response tag mismatch or unexpected response: {other:?}"
            ))),
        }
    }

    pub(super) fn call_op_timeout(&self, op: Op, timeout: Duration) -> Result<Completion> {
        self.call_op_timeout_inner(op, timeout, |tag| self.observe_call(tag), true)
    }

    pub fn call_timeout<F, G, H>(
        &self,
        build: F,
        timeout: Duration,
        observe: G,
    ) -> Result<Completion>
    where
        F: FnOnce(&mut ProtocolClient) -> Result<Op>,
        G: FnOnce(Tag) -> H,
    {
        let op = {
            let mut protocol = lock(&self.inner.protocol, "lock 9P protocol client")?;
            build(&mut protocol).map_err(protocol_error)?
        };
        self.call_op_timeout_inner(op, timeout, observe, true)
    }

    pub(super) fn call_op_timeout_inner<G, H>(
        &self,
        op: Op,
        timeout: Duration,
        observe: G,
        flush_on_timeout: bool,
    ) -> Result<Completion>
    where
        G: FnOnce(Tag) -> H,
    {
        let expected_tag = op.tag;
        let pending = self.submit_op(op)?;
        let _guard = observe(expected_tag);
        self.wait_pending_timeout(pending, timeout, flush_on_timeout)
    }

    pub(super) fn wait_pending_timeout(
        &self,
        pending: PendingCall,
        timeout: Duration,
        flush_on_timeout: bool,
    ) -> Result<Completion> {
        let expected_tag = pending.tag;
        match self.wait_pending_response_timeout(pending, timeout, flush_on_timeout)? {
            ClientResponse::Completion { tag, completion } if tag == expected_tag => Ok(completion),
            ClientResponse::Error { tag, ename } if tag == expected_tag => Err(Error::new(ename)),
            other => Err(Error::from(format!(
                "response tag mismatch or unexpected response: {other:?}"
            ))),
        }
    }

    pub(super) fn wait_pending_response_timeout(
        &self,
        pending: PendingCall,
        timeout: Duration,
        flush_on_timeout: bool,
    ) -> Result<ClientResponse> {
        let expected_tag = pending.tag;
        let response = match pending.receiver.recv_timeout(timeout) {
            Ok(response) => response?,
            Err(RecvTimeoutError::Timeout) => {
                if flush_on_timeout {
                    let _ = self.flush_tag_timeout(expected_tag, bounded_flush_timeout(timeout));
                }
                self.cancel_waiter(
                    expected_tag,
                    Error::from(format!(
                        "9P response timeout after {:.3}s",
                        timeout.as_secs_f64()
                    )),
                );
                return Err(Error::from(format!(
                    "9P response timeout after {:.3}s",
                    timeout.as_secs_f64()
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::from("9P reader stopped before response"));
            }
        };
        Ok(response)
    }

    pub(super) fn cancel_waiter(&self, tag: Tag, error: Error) {
        let sender = lock(&self.inner.responses, "lock 9P response state")
            .ok()
            .and_then(|mut responses| responses.remove(tag));
        if let Some(sender) = sender {
            let _ = sender.send(Err(error));
        }
    }

    pub(super) fn observe_call(&self, tag: Tag) -> CallObserverGuard {
        match &self.call_observer {
            Some(observer) => observer(tag),
            None => Box::new(()),
        }
    }
}
