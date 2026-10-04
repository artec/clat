//! D2's first consumer discriminator; this harness performs no DNS/HTTP I/O.
use super::*;

mod probe {
    wasmtime::component::bindgen!({
        path: "../../sdk/wit-proposals/net",
        world: "consumer-probe",
    });
}
use probe::clat::net::egress::{Address, Failure, Header, Request, Resolution, Response};

struct HeldPhase {
    phase: String,
    held: usize,
}

impl HeldPhase {
    fn hold(&mut self, phase: &str) {
        if self.phase == phase {
            self.held += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl probe::clat::net::egress::Host for HeldPhase {
    fn dns_resolve(
        &mut self,
        _origin: String,
        _timeout: u32,
    ) -> Result<Resource<Resolution>, Failure> {
        self.hold("dns");
        Ok(Resource::new_own(0))
    }

    fn http_request(
        &mut self,
        _target: Resource<Resolution>,
        _request: Request,
    ) -> Result<Resource<Response>, Failure> {
        self.hold("headers");
        Ok(Resource::new_own(0))
    }
}

impl probe::clat::net::egress::HostResolution for HeldPhase {
    fn addresses(&mut self, _: Resource<Resolution>) -> Result<Vec<Address>, Failure> {
        Ok(Vec::new())
    }
    fn dns64_answers(&mut self, _: Resource<Resolution>) -> Result<Vec<Address>, Failure> {
        Ok(Vec::new())
    }
    fn drop(&mut self, _: Resource<Resolution>) -> wasmtime::Result<()> {
        Ok(())
    }
}

impl probe::clat::net::egress::HostResponse for HeldPhase {
    fn status(&mut self, _: Resource<Response>) -> Result<u16, Failure> {
        Ok(200)
    }
    fn headers(&mut self, _: Resource<Response>) -> Result<Vec<Header>, Failure> {
        Ok(Vec::new())
    }
    fn read_body(&mut self, _: Resource<Response>, _: u32) -> Result<Vec<u8>, Failure> {
        self.hold("body");
        Ok(Vec::new())
    }
    fn cancel(&mut self, _: Resource<Response>) {}
    fn drop(&mut self, _: Resource<Response>) -> wasmtime::Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "author component; D2 async consumer pre-red before any mapping layer"]
fn plg4_sync_import_cannot_deliver_guest_abort() {
    let Some(path) = std::env::var_os("CLAT_PLG4_SYNC_COMPONENT") else {
        return;
    };
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let component = wasmtime::component::Component::from_binary(&engine, &bytes).unwrap();
    let mut linker = Linker::new(&engine);
    probe::ConsumerProbe::add_to_linker::<HeldPhase, wasmtime::component::HasSelf<HeldPhase>>(
        &mut linker,
        |state| state,
    )
    .unwrap();
    let require_async = std::env::var_os("CLAT_PLG4_REQUIRE_ASYNC").is_some();
    let mut abort_during_wait = Vec::new();
    for phase in ["dns", "headers", "body"] {
        let mut store = Store::new(
            &engine,
            HeldPhase {
                phase: phase.into(),
                held: 0,
            },
        );
        store.set_fuel(CALL_FUEL).unwrap();
        let instance = probe::ConsumerProbe::instantiate(&mut store, &component, &linker).unwrap();
        let result: Value =
            serde_json::from_str(&instance.call_run(&mut store, phase).unwrap()).unwrap();
        assert_eq!(store.data().held, 1);
        assert_eq!(result["aborted"], true);
        eprintln!("PLG4_SYNC {result}");
        assert_eq!(
            result["trace"],
            serde_json::json!(["enter", "return", "abort"])
        );
        abort_during_wait.push(result["trace"] == serde_json::json!(["enter", "abort", "return"]));
    }
    if require_async {
        assert!(
            abort_during_wait.into_iter().all(|delivered| delivered),
            "D2 consumer pre-red: abort must be deliverable while the host waits in DNS, headers and body"
        );
    }
}
