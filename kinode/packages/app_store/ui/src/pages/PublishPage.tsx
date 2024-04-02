import React, { useState, useCallback, FormEvent, useEffect } from "react";
import { useLocation } from "react-router-dom";
import { BigNumber, utils } from "ethers";
import { useWeb3React } from "@web3-react/core";

import SearchHeader from "../components/SearchHeader";
import { PageProps } from "../types/Page";
import { setChain } from "../utils/chain";
import { SEPOLIA_OPT_HEX } from "../constants/chain";
import { hooks, metaMask } from "../utils/metamask";
import Loader from "../components/Loader";
import { toDNSWireFormat } from "../utils/dnsWire";
import useAppsStore from "../store/apps-store";
import MetadataForm from "../components/MetadataForm";
import { AppInfo } from "../types/Apps";
import Checkbox from "../components/Checkbox";

const { useIsActivating } = hooks;

interface PublishPageProps extends PageProps {}

export default function PublishPage({
  provider,
  packageAbi,
}: PublishPageProps) {
  // get state from router
  const { state } = useLocation();
  const { listedApps } = useAppsStore();
  // TODO: figure out how to handle provider
  const { account, isActive } = useWeb3React();
  const isActivating = useIsActivating();

  const [loading, setLoading] = useState("");
  const [publishSuccess, setPublishSuccess] = useState<
    { packageName: string; publisherId: string } | undefined
  >();
  const [showMetadataForm, setShowMetadataForm] = useState<boolean>(false);
  const [packageName, setPackageName] = useState<string>("");
  const [publisherId, setPublisherId] = useState<string>(
    window.our?.node || ""
  ); // BytesLike
  const [metadataUrl, setMetadataUrl] = useState<string>("");
  const [metadataHash, setMetadataHash] = useState<string>(""); // BytesLike
  const [isUpdate, setIsUpdate] = useState<boolean>(false);

  useEffect(() => {
    const app: AppInfo | undefined = state?.app;
    if (app) {
      setPackageName(app.package);
      setPublisherId(app.publisher);
      setIsUpdate(true);
    }
  }, [state])

  const connectWallet = useCallback(async () => {
    await metaMask.activate().catch(() => {});

    try {
      setChain(SEPOLIA_OPT_HEX);
    } catch (error) {
      console.error(error);
    }
  }, []);

  const calculateMetadataHash = useCallback(async () => {
    if (!metadataUrl) {
      setMetadataHash("");
      return;
    }
    try {
      const metadataResponse = await fetch(metadataUrl);
      const metadataText = await metadataResponse.text();
      JSON.parse(metadataText); // confirm it's valid JSON
      const metadataHash = utils.keccak256(utils.toUtf8Bytes(metadataText));
      setMetadataHash(metadataHash);
    } catch (error) {
      window.alert(
        "Error calculating metadata hash. Please ensure the URL is valid and the metadata is in JSON format."
      );
    }
  }, [metadataUrl]);

  const publishPackage = useCallback(
    async (e: FormEvent<HTMLFormElement>) => {
      e.preventDefault();
      e.stopPropagation();

      let metadata = metadataHash;

      try {
        if (!metadata) {
          // https://pongo-uploads.s3.us-east-2.amazonaws.com/chat_metadata.json
          const metadataResponse = await fetch(metadataUrl);
          await metadataResponse.json(); // confirm it's valid JSON
          const metadataText = await metadataResponse.text(); // hash as text
          metadata = utils.keccak256(utils.toUtf8Bytes(metadataText));
        }

        setLoading("Please confirm the transaction in your wallet");
        const publisherIdDnsWireFormat = toDNSWireFormat(publisherId);
        await setChain(SEPOLIA_OPT_HEX);

        // TODO: have a checkbox to show if it's an update of an existing package

        const tx = await (isUpdate
          ? packageAbi.updateMetadata(
              BigNumber.from(
                utils.solidityKeccak256(
                  ["string", "bytes"],
                  [packageName, publisherIdDnsWireFormat]
                )
              ),
              metadataUrl,
              metadata
            )
          : packageAbi.registerApp(
              packageName,
              publisherIdDnsWireFormat,
              metadataUrl,
              metadata
            ));

        await new Promise((resolve) => setTimeout(resolve, 2000));

        setLoading("Publishing package...");
        await tx.wait();
        setPublishSuccess({ packageName, publisherId });
        setPackageName("");
        setPublisherId(window.our?.node || publisherId);
        setMetadataUrl("");
        setMetadataHash("");
        setIsUpdate(false);
      } catch (error) {
        console.error(error);
        window.alert(
          "Error publishing package. Please ensure the package name and publisher ID are valid, and the metadata is in JSON format."
        );
      } finally {
        setLoading("");
      }
    },
    [
      packageName,
      isUpdate,
      publisherId,
      metadataUrl,
      metadataHash,
      packageAbi,
      setPublishSuccess,
      setPackageName,
      setPublisherId,
      setMetadataUrl,
      setMetadataHash,
      setIsUpdate,
    ]
  );

  const checkIfUpdate = useCallback(async () => {
    if (isUpdate) return;

    if (
      packageName &&
      publisherId &&
      listedApps.find(
        (app) => app.package === packageName && app.publisher === publisherId
      )
    ) {
      setIsUpdate(true);
    }
  }, [listedApps, packageName, publisherId, isUpdate, setIsUpdate]);

  return (
    <div style={{ width: "100%" }}>
      <SearchHeader hideSearch onBack={showMetadataForm ? () => setShowMetadataForm(false) : undefined} />
      <div className="row between page-title">
        <h4>Publish Package</h4>
        {Boolean(account) && (
          <div style={{ textAlign: "right", lineHeight: 1.5 }}>
            {" "}
            Connected as{" "}
            {account?.slice(0, 6) + "..." + account?.slice(account.length - 6)}
          </div>
        )}
      </div>

      {loading ? (
        <div className="col center">
          <Loader msg={loading} />
        </div>
      ) : publishSuccess ? (
        <div className="col center">
          <h4 style={{ marginBottom: "0.5em" }}>Package Published!</h4>
          <div style={{ marginBottom: "0.5em" }}>
            <strong>Package Name:</strong> {publishSuccess.packageName}
          </div>
          <div style={{ marginBottom: "0.5em" }}>
            <strong>Publisher ID:</strong> {publishSuccess.publisherId}
          </div>
          <button
            className={`my-pkg-btn row`}
            style={{ marginTop: "1em" }}
            onClick={() => setPublishSuccess(undefined)}
          >
            Publish Another Package
          </button>
        </div>
      ) : showMetadataForm ? (
        <MetadataForm {...{packageName, publisherId, app: state?.app}} goBack={() => setShowMetadataForm(false)} />
      ) : !account || !isActive ? (
        <>
          <h4 style={{}}>Please connect your wallet to publish a package</h4>
          <button className={`connect-wallet row`} onClick={connectWallet}>
            Connect Wallet
          </button>
        </>
      ) : isActivating ? (
        <Loader msg="Approve connection in your wallet" />
      ) : (
        <form
          className="new card col"
          style={{ flex: 1, overflowY: "scroll" }}
          onSubmit={publishPackage}
        >
          <div
            className="row between"
            style={{
              cursor: "pointer",
              padding: "0.5em",
              margin: "0 0 0 -0.5em",
            }}
            onClick={() => setIsUpdate(!isUpdate)}
          >
            <Checkbox checked={isUpdate} readOnly />
            <label htmlFor="update" style={{ cursor: "pointer", marginLeft: 8 }}>
              Update existing package
            </label>
          </div>
          <div className="col f-width">
            <label htmlFor="package-name">Package Name</label>
            <input
              style={{ minWidth: "80%" }}
              id="package-name"
              type="text"
              required
              placeholder="my-package"
              value={packageName}
              onChange={(e) => setPackageName(e.target.value)}
              onBlur={checkIfUpdate}
            />
          </div>
          <div className="col f-width">
            <label htmlFor="publisher-id">Publisher ID</label>
            <input
              style={{ minWidth: "80%" }}
              id="publisher-id"
              type="text"
              required
              value={publisherId}
              onChange={(e) => setPublisherId(e.target.value)}
              onBlur={checkIfUpdate}
            />
          </div>
          <div className="col f-width">
            <label htmlFor="metadata-url">
              Metadata URL
            </label>
            <input
              style={{ minWidth: "80%" }}
              id="metadata-url"
              type="text"
              required
              value={metadataUrl}
              onChange={(e) => setMetadataUrl(e.target.value)}
              onBlur={calculateMetadataHash}
              placeholder="https://github/my-org/my-repo/metadata.json"
            />
            <div style={{ textAlign: "left", margin: "0.5em 0 0" }}>
                Metadata is a JSON file that describes your package.
                <br /> You can{" "}
                <a onClick={() => setShowMetadataForm(true)} style={{ cursor: "pointer", textDecoration: "underline" }}>
                  fill out a template here
                </a>
                .
              </div>
          </div>
          <div className="col f-width">
            <label htmlFor="metadata-hash">Metadata Hash</label>
            <input
              style={{ minWidth: "80%" }}
              readOnly
              id="metadata-hash"
              type="text"
              value={metadataHash}
              onChange={(e) => setMetadataHash(e.target.value)}
              placeholder="Calculated automatically from metadata URL"
            />
          </div>
          <button type="submit" className="primary">
            Publish
          </button>
        </form>
      )}
    </div>
  );
}
